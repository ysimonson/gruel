//! AIR to X86Mir lowering.
//!
//! This phase converts AIR (typed, high-level IR) to X86Mir (x86-64 instructions
//! with virtual registers). The lowering is demand-driven: instructions are only
//! lowered when their values are needed, enabling short-circuit evaluation of
//! logical operators (&&, ||).

use std::collections::HashMap;

use rue_air::{Air, AirInstData, AirRef, StructId, Type};

use super::mir::{Operand, Reg, VReg, X86Inst, X86Mir};

/// Context for the current loop being lowered.
/// Used to support break and continue statements.
struct LoopContext {
    /// Label to jump to for `continue` (loop start, where condition is re-evaluated).
    continue_label: String,
    /// Label to jump to for `break` (loop end, exits the loop).
    break_label: String,
}

/// AIR to X86Mir lowering.
pub struct Lower<'a> {
    air: &'a Air,
    mir: X86Mir,
    /// Maps AIR instruction refs to the vreg holding their result.
    value_map: Vec<Option<VReg>>,
    /// Label counter for generating unique labels.
    label_counter: u32,
    /// Whether this function has a stack frame (num_locals > 0 or num_params > 0).
    /// Used to emit proper epilogue before returns.
    has_frame: bool,
    /// Number of local variable slots.
    num_locals: u32,
    /// Number of ABI parameter slots for this function.
    num_params: u32,
    /// Function name for this function (for generating internal labels).
    fn_name: String,
    /// Stack of loop contexts for nested loops.
    /// Used to resolve break and continue targets.
    loop_stack: Vec<LoopContext>,
    /// Maps StructInit AIR refs to their field vregs.
    /// Used by Alloc to store all struct fields to consecutive stack slots.
    struct_field_vregs: HashMap<AirRef, Vec<VReg>>,
    /// Maps StructId to field count. Populated lazily from AIR instructions.
    struct_field_counts: HashMap<StructId, u32>,
}

/// Argument passing registers per System V AMD64 ABI.
const ARG_REGS: [Reg; 6] = [Reg::Rdi, Reg::Rsi, Reg::Rdx, Reg::Rcx, Reg::R8, Reg::R9];

impl<'a> Lower<'a> {
    /// Create a new lowering pass.
    pub fn new(air: &'a Air, num_locals: u32, num_params: u32, fn_name: &str) -> Self {
        // Collect struct field counts from StructInit and FieldGet instructions in AIR
        let mut struct_field_counts: HashMap<StructId, u32> = HashMap::new();
        for (_, inst) in air.iter() {
            match &inst.data {
                AirInstData::StructInit { struct_id, fields } => {
                    struct_field_counts.insert(*struct_id, fields.len() as u32);
                }
                AirInstData::FieldGet { struct_id, field_index, .. } => {
                    // Update if we see a higher field index
                    let entry = struct_field_counts.entry(*struct_id).or_insert(0);
                    *entry = (*entry).max(*field_index + 1);
                }
                AirInstData::FieldSet { struct_id, field_index, .. } => {
                    let entry = struct_field_counts.entry(*struct_id).or_insert(0);
                    *entry = (*entry).max(*field_index + 1);
                }
                _ => {}
            }
        }

        Self {
            air,
            mir: X86Mir::new(),
            value_map: vec![None; air.len()],
            label_counter: 0,
            has_frame: num_locals > 0 || num_params > 0,
            num_locals,
            num_params,
            fn_name: fn_name.to_string(),
            loop_stack: Vec::new(),
            struct_field_vregs: HashMap::new(),
            struct_field_counts,
        }
    }

    /// Get the number of fields for a struct type.
    fn struct_field_count(&self, struct_id: StructId) -> u32 {
        *self.struct_field_counts.get(&struct_id).unwrap_or(&1)
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
    /// Labels are globally unique by including the function name and a counter.
    /// Format: .L{fn_name}_{prefix}_{counter}
    /// This ensures labels don't collide across multiple functions in the same
    /// object file.
    fn new_label(&mut self, prefix: &str) -> String {
        let label = format!(".L{}_{}_{}", self.fn_name, prefix, self.label_counter);
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
                // Note: The register allocator only uses callee-saved registers (R12-R15, RBX),
                // avoiding RAX/RDX entirely. If the allocator is expanded to use more registers,
                // it can use X86Inst::clobbers() and LivenessInfo::is_clobbered_during() to
                // avoid placing live values in registers that would be clobbered.
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
                // Check if the init is a StructInit - if so, store all fields
                // to consecutive slots instead of just the first field.
                let init_data = self.air.get(*init).data.clone();
                if let AirInstData::StructInit { .. } = &init_data {
                    // Lower the StructInit first to get all field vregs
                    self.demand_lower(*init);

                    // Get the field vregs that were saved by StructInit
                    if let Some(field_vregs) = self.struct_field_vregs.get(init).cloned() {
                        // Store each field to consecutive slots
                        for (i, field_vreg) in field_vregs.iter().enumerate() {
                            let field_slot = slot + i as u32;
                            let offset = self.local_offset(field_slot);
                            self.mir.push(X86Inst::MovMR {
                                base: Reg::Rbp,
                                offset,
                                src: Operand::Virtual(*field_vreg),
                            });
                        }
                    } else {
                        // Fallback: just use the single vreg (shouldn't happen)
                        let init_vreg = self.value_map[init.as_u32() as usize]
                            .expect("StructInit should be lowered");
                        let offset = self.local_offset(*slot);
                        self.mir.push(X86Inst::MovMR {
                            base: Reg::Rbp,
                            offset,
                            src: Operand::Virtual(init_vreg),
                        });
                    }
                } else {
                    // Regular (non-struct) value
                    let init_vreg = self.get_vreg(*init);

                    // Store to stack slot
                    let offset = self.local_offset(*slot);
                    self.mir.push(X86Inst::MovMR {
                        base: Reg::Rbp,
                        offset,
                        src: Operand::Virtual(init_vreg),
                    });
                }

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

            AirInstData::Loop { cond, body } => {
                // While loop: while cond { body }
                //
                // The challenge is that the condition and body may reference values
                // that need to be re-evaluated each iteration. We must lower the
                // condition INSIDE the loop so it's re-computed each time.
                //
                // Structure:
                //   loop_start:
                //     evaluate condition (freshly each iteration)
                //     if false, jump to loop_end
                //     evaluate body
                //     jump to loop_start
                //   loop_end:

                let loop_start = self.new_label("loop_start");
                let loop_end = self.new_label("loop_end");

                // Push the loop context for break/continue support
                self.loop_stack.push(LoopContext {
                    continue_label: loop_start.clone(),
                    break_label: loop_end.clone(),
                });

                // Loop start label
                self.mir.push(X86Inst::Label { name: loop_start.clone() });

                // Evaluate condition fresh each iteration
                // We need to re-lower it each time, but since we're in a loop,
                // we need to clear the value_map entries for the condition
                // and body so they get re-computed.
                // For now, just demand_lower the condition - it will use cached
                // values for things computed outside the loop, but we need the
                // Load instructions inside the condition to be re-evaluated.
                // Actually, the current design assumes each AIR instruction is
                // lowered once. For loops, we need a different approach.
                //
                // Solution: Generate the condition check inline each iteration
                // by re-lowering the condition instructions.
                self.demand_lower(*cond);
                let cond_vreg = self.value_map[cond.as_u32() as usize]
                    .expect("condition should be lowered");

                // If condition is false (zero), exit loop
                self.mir.push(X86Inst::CmpRI {
                    src: Operand::Virtual(cond_vreg),
                    imm: 0,
                });
                self.mir.push(X86Inst::Jz { label: loop_end.clone() });

                // Execute body
                self.demand_lower(*body);

                // Before jumping back, clear the value_map for instructions that
                // need to be re-evaluated (loads from mutable variables).
                // For now, we'll take the simpler approach: just jump back and
                // rely on the fact that demand_lower will re-execute instructions
                // if they haven't been done yet... but wait, they HAVE been done.
                //
                // The real fix is that Load instructions from mutable slots
                // should be re-executed each iteration. But our current model
                // doesn't support that - each AIR ref is lowered exactly once.
                //
                // Workaround: Clear the value_map entries for instructions that
                // are inside the loop's condition and body so they get re-lowered.
                // This is a hack but will work for now.
                self.clear_loop_values(*cond, *body);

                // Jump back to start
                self.mir.push(X86Inst::Jmp { label: loop_start });

                // Loop end label
                self.mir.push(X86Inst::Label { name: loop_end });

                // Pop the loop context now that the loop is done
                self.loop_stack.pop();

                // Loop doesn't produce a value (Unit type), but we need something
                // in the value_map. Use a dummy vreg with 0.
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);
                self.mir.push(X86Inst::MovRI32 {
                    dst: Operand::Virtual(vreg),
                    imm: 0,
                });
            }

            AirInstData::Break => {
                // Break: exit the innermost loop by jumping to its end label.
                // The loop context was validated by Sema, so we know we're in a loop.
                let ctx = self.loop_stack.last().expect("break outside loop should be caught by sema");
                let break_label = ctx.break_label.clone();

                // Jump to loop end. No vreg allocation needed - break is a diverging
                // control flow statement, so any code after it is unreachable.
                self.mir.push(X86Inst::Jmp { label: break_label });
            }

            AirInstData::Continue => {
                // Continue: skip to the next iteration by jumping to the loop start.
                // We DON'T clear loop values here - that's done at the end of each iteration.
                // The values will be cleared when we reach the jump back at the normal
                // end of the loop body. If we cleared here, we'd corrupt the ongoing
                // lowering process.
                let ctx = self.loop_stack.last().expect("continue outside loop should be caught by sema");
                let continue_label = ctx.continue_label.clone();

                // Jump to loop start (condition check). No vreg allocation needed -
                // continue is a diverging control flow statement.
                self.mir.push(X86Inst::Jmp { label: continue_label });
            }

            AirInstData::Ret(value_ref) => {
                // Get the vreg holding the return value
                let value_vreg = self.get_vreg(*value_ref);

                if self.fn_name == "main" {
                    // Main function: call __rue_exit with the return value
                    // Move return value to rdi (first argument per System V AMD64 ABI).
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rdi),
                        src: Operand::Virtual(value_vreg),
                    });

                    // Emit epilogue to restore stack frame before the call.
                    if self.has_frame {
                        self.emit_epilogue();
                    }

                    // Call the runtime's __rue_exit function.
                    // This function never returns (it calls the exit syscall).
                    self.mir.push(X86Inst::CallRel {
                        symbol: "__rue_exit".to_string(),
                    });
                } else {
                    // Non-main function: return normally with value in RAX
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rax),
                        src: Operand::Virtual(value_vreg),
                    });

                    // Emit epilogue to restore stack frame
                    if self.has_frame {
                        self.emit_epilogue();
                    }

                    // Return to caller
                    self.mir.push(X86Inst::Ret);
                }
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

                // The block's value is the result.
                // If it's a Unit-typed instruction (Store, Alloc), it doesn't produce
                // a vreg, so we just demand_lower it and use a dummy value.
                let value_inst = self.air.get(*value);
                if value_inst.ty == Type::Unit {
                    // Unit type - just execute for side effects, use dummy vreg
                    self.demand_lower(*value);
                    let vreg = self.mir.alloc_vreg();
                    self.value_map[air_ref.as_u32() as usize] = Some(vreg);
                    self.mir.push(X86Inst::MovRI32 {
                        dst: Operand::Virtual(vreg),
                        imm: 0,
                    });
                } else {
                    // Has a value - use it
                    let value_vreg = self.get_vreg(*value);
                    self.value_map[air_ref.as_u32() as usize] = Some(value_vreg);
                }
            }

            AirInstData::Param { index } => {
                // Parameters are saved to the stack in the function prologue.
                // They are stored at slots num_locals + index (after local variables).
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                if (*index as usize) < ARG_REGS.len() {
                    // Parameter was in a register, now saved to stack at slot num_locals + index
                    let slot = self.num_locals + *index;
                    let offset = self.local_offset(slot);
                    self.mir.push(X86Inst::MovRM {
                        dst: Operand::Virtual(vreg),
                        base: Reg::Rbp,
                        offset,
                    });
                } else {
                    // Parameter is on the stack (beyond first 6)
                    // Stack layout after call: [return addr][arg7][arg8]...
                    // After push rbp: [saved rbp][return addr][arg7][arg8]...
                    // So arg7 is at [rbp + 16], arg8 at [rbp + 24], etc.
                    let stack_offset = 16 + ((*index as i32) - 6) * 8;
                    self.mir.push(X86Inst::MovRM {
                        dst: Operand::Virtual(vreg),
                        base: Reg::Rbp,
                        offset: stack_offset,
                    });
                }
            }

            AirInstData::Call { name, args } => {
                // Function call using System V AMD64 ABI
                // First 6 args go in registers: RDI, RSI, RDX, RCX, R8, R9
                // Remaining args are passed on the stack (right-to-left)
                //
                // For struct arguments, each field is passed as a separate ABI argument.
                let result_vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(result_vreg);

                // Flatten all arguments: for structs, collect each field as a separate ABI arg.
                let mut flattened_vregs: Vec<VReg> = Vec::new();
                for arg in args {
                    let arg_type = self.air.get(*arg).ty;
                    match arg_type {
                        Type::Struct(_struct_id) => {
                            // Struct argument: need to pass all fields as separate ABI args.
                            // Look at the AIR instruction to determine how to get the fields.
                            let arg_data = self.air.get(*arg).data.clone();
                            match &arg_data {
                                AirInstData::Load { slot } => {
                                    // Struct from local variable: load each field from consecutive slots
                                    let field_count = match arg_type {
                                        Type::Struct(sid) => self.struct_field_count(sid),
                                        _ => unreachable!(),
                                    };
                                    // Load each field into a vreg
                                    for field_idx in 0..field_count {
                                        let field_vreg = self.mir.alloc_vreg();
                                        let field_slot = slot + field_idx;
                                        let offset = self.local_offset(field_slot);
                                        self.mir.push(X86Inst::MovRM {
                                            dst: Operand::Virtual(field_vreg),
                                            base: Reg::Rbp,
                                            offset,
                                        });
                                        flattened_vregs.push(field_vreg);
                                    }
                                }
                                AirInstData::Param { index } => {
                                    // Struct from parameter: load each field from consecutive param slots
                                    let field_count = match arg_type {
                                        Type::Struct(sid) => self.struct_field_count(sid),
                                        _ => unreachable!(),
                                    };
                                    for field_idx in 0..field_count {
                                        let field_vreg = self.mir.alloc_vreg();
                                        let param_slot = self.num_locals + index + field_idx;
                                        let offset = self.local_offset(param_slot);
                                        self.mir.push(X86Inst::MovRM {
                                            dst: Operand::Virtual(field_vreg),
                                            base: Reg::Rbp,
                                            offset,
                                        });
                                        flattened_vregs.push(field_vreg);
                                    }
                                }
                                AirInstData::StructInit { .. } => {
                                    // Struct literal: get field vregs from struct_field_vregs
                                    // First ensure the struct init is lowered
                                    self.demand_lower(*arg);
                                    if let Some(field_vregs) = self.struct_field_vregs.get(arg) {
                                        flattened_vregs.extend(field_vregs.iter().copied());
                                    } else {
                                        // Fallback: just use the single vreg
                                        flattened_vregs.push(self.get_vreg(*arg));
                                    }
                                }
                                _ => {
                                    // Other struct expression: just use single vreg (may not work correctly)
                                    flattened_vregs.push(self.get_vreg(*arg));
                                }
                            }
                        }
                        _ => {
                            // Non-struct argument: single vreg
                            flattened_vregs.push(self.get_vreg(*arg));
                        }
                    }
                }

                let num_reg_args = flattened_vregs.len().min(ARG_REGS.len());
                let num_stack_args = flattened_vregs.len().saturating_sub(ARG_REGS.len());

                // Phase 1: Push stack arguments (args 7+) in reverse order
                // Per System V AMD64 ABI, stack args are pushed right-to-left
                // so that arg7 ends up closest to RSP
                for arg_vreg in flattened_vregs.iter().skip(ARG_REGS.len()).rev() {
                    // Move to RAX first (in case vreg is spilled)
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rax),
                        src: Operand::Virtual(*arg_vreg),
                    });
                    // Push RAX
                    self.mir.push(X86Inst::Push {
                        src: Operand::Physical(Reg::Rax),
                    });
                }

                // Phase 2: Push register arguments onto stack temporarily
                // This avoids clobbering issues when vregs are in arg registers
                for arg_vreg in flattened_vregs.iter().take(num_reg_args).rev() {
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Physical(Reg::Rax),
                        src: Operand::Virtual(*arg_vreg),
                    });
                    self.mir.push(X86Inst::Push {
                        src: Operand::Physical(Reg::Rax),
                    });
                }

                // Phase 3: Pop into argument registers (forward order)
                for i in 0..num_reg_args {
                    self.mir.push(X86Inst::Pop {
                        dst: Operand::Physical(ARG_REGS[i]),
                    });
                }

                // Call the function
                self.mir.push(X86Inst::CallRel {
                    symbol: name.clone(),
                });

                // Clean up stack arguments (if any)
                if num_stack_args > 0 {
                    let stack_space = (num_stack_args * 8) as i32;
                    self.mir.push(X86Inst::AddRI {
                        dst: Operand::Physical(Reg::Rsp),
                        imm: stack_space,
                    });
                }

                // Return value is in RAX - move to result vreg
                self.mir.push(X86Inst::MovRR {
                    dst: Operand::Virtual(result_vreg),
                    src: Operand::Physical(Reg::Rax),
                });
            }

            AirInstData::StructInit { struct_id: _, fields } => {
                // Struct initialization: evaluate all fields and save their vregs.
                // The actual storage to stack slots is handled by Alloc.
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                // Evaluate all fields to get their vregs
                let mut field_vregs = Vec::new();
                for field in fields {
                    let field_vreg = self.get_vreg(*field);
                    field_vregs.push(field_vreg);
                }

                // Use the first field as the representative value (for simple cases)
                if let Some(&first_vreg) = field_vregs.first() {
                    self.mir.push(X86Inst::MovRR {
                        dst: Operand::Virtual(vreg),
                        src: Operand::Virtual(first_vreg),
                    });
                } else {
                    // Empty struct - just set to 0
                    self.mir.push(X86Inst::MovRI32 {
                        dst: Operand::Virtual(vreg),
                        imm: 0,
                    });
                }

                // Save field vregs for Alloc to pick up
                self.struct_field_vregs.insert(air_ref, field_vregs);
            }

            AirInstData::FieldGet { base, struct_id: _, field_index } => {
                // Field access: load from base_slot + field_index.
                // The base can be:
                // - Load: struct is a local variable
                // - Param: struct is a function parameter
                let vreg = self.mir.alloc_vreg();
                self.value_map[air_ref.as_u32() as usize] = Some(vreg);

                // Look at the base instruction to find the slot
                let base_data = self.air.get(*base).data.clone();
                match &base_data {
                    AirInstData::Load { slot } => {
                        // Local variable: load from slot + field_index
                        let actual_slot = slot + field_index;
                        let offset = self.local_offset(actual_slot);
                        self.mir.push(X86Inst::MovRM {
                            dst: Operand::Virtual(vreg),
                            base: Reg::Rbp,
                            offset,
                        });
                    }
                    AirInstData::Param { index } => {
                        // Struct parameter: load from param_slot + field_index.
                        // Parameters are stored at [num_locals + index] in the stack frame.
                        let param_slot = self.num_locals + index + field_index;
                        let offset = self.local_offset(param_slot);
                        self.mir.push(X86Inst::MovRM {
                            dst: Operand::Virtual(vreg),
                            base: Reg::Rbp,
                            offset,
                        });
                    }
                    _ => {
                        // Base is some other expression - this shouldn't happen
                        // for well-formed struct field access on a local variable.
                        // Just evaluate it and use directly (won't work correctly
                        // for multi-field structs, but provides a fallback).
                        let base_vreg = self.get_vreg(*base);
                        self.mir.push(X86Inst::MovRR {
                            dst: Operand::Virtual(vreg),
                            src: Operand::Virtual(base_vreg),
                        });
                    }
                }
            }

            AirInstData::FieldSet { slot, struct_id: _, field_index, value } => {
                // Field assignment: store value to slot + field_index
                let value_vreg = self.get_vreg(*value);

                let actual_slot = slot + field_index;
                let offset = self.local_offset(actual_slot);
                self.mir.push(X86Inst::MovMR {
                    base: Reg::Rbp,
                    offset,
                    src: Operand::Virtual(value_vreg),
                });

                // FieldSet doesn't produce a value
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

    /// Clear value_map entries for instructions in a loop so they can be re-lowered.
    /// This is needed because load/store operations inside a loop need to execute
    /// each iteration, not just once.
    fn clear_loop_values(&mut self, cond: AirRef, body: AirRef) {
        // Clear the condition and all instructions it depends on that might change
        self.clear_transitive_deps(cond);

        // Clear the body
        self.clear_transitive_deps(body);
    }

    /// Recursively clear an instruction and its dependencies from value_map.
    /// Constants and allocs are NOT cleared since they don't need re-evaluation.
    fn clear_transitive_deps(&mut self, air_ref: AirRef) {
        // Get the instruction data first to check what type it is
        let data = self.air.get(air_ref).data.clone();

        // Don't clear constants, params, or allocs - they don't change between iterations
        match &data {
            AirInstData::Const(_)
            | AirInstData::BoolConst(_)
            | AirInstData::Param { .. }
            | AirInstData::Alloc { .. } => return,
            _ => {}
        }

        // Clear this instruction
        self.value_map[air_ref.as_u32() as usize] = None;

        // Clear dependencies too (recursively)
        match data {
            AirInstData::Load { .. } => {
                // Load from a slot - this MUST be re-executed each iteration
                // Already cleared above, no dependencies to clear
            }
            AirInstData::Store { value, .. } => {
                self.clear_transitive_deps(value);
            }
            AirInstData::Add(lhs, rhs)
            | AirInstData::Sub(lhs, rhs)
            | AirInstData::Mul(lhs, rhs)
            | AirInstData::Div(lhs, rhs)
            | AirInstData::Mod(lhs, rhs)
            | AirInstData::Eq(lhs, rhs)
            | AirInstData::Ne(lhs, rhs)
            | AirInstData::Lt(lhs, rhs)
            | AirInstData::Gt(lhs, rhs)
            | AirInstData::Le(lhs, rhs)
            | AirInstData::Ge(lhs, rhs)
            | AirInstData::And(lhs, rhs)
            | AirInstData::Or(lhs, rhs) => {
                self.clear_transitive_deps(lhs);
                self.clear_transitive_deps(rhs);
            }
            AirInstData::Neg(operand) | AirInstData::Not(operand) => {
                self.clear_transitive_deps(operand);
            }
            AirInstData::Block { statements, value } => {
                for stmt in statements {
                    self.clear_transitive_deps(stmt);
                }
                self.clear_transitive_deps(value);
            }
            AirInstData::Branch { cond, then_value, else_value } => {
                self.clear_transitive_deps(cond);
                self.clear_transitive_deps(then_value);
                if let Some(else_v) = else_value {
                    self.clear_transitive_deps(else_v);
                }
            }
            AirInstData::Loop { cond, body } => {
                self.clear_transitive_deps(cond);
                self.clear_transitive_deps(body);
            }
            // Struct operations
            AirInstData::StructInit { fields, .. } => {
                for field in fields {
                    self.clear_transitive_deps(field);
                }
            }
            AirInstData::FieldGet { base, .. } => {
                self.clear_transitive_deps(base);
            }
            AirInstData::FieldSet { value, .. } => {
                self.clear_transitive_deps(value);
            }
            // These were already handled by the early return above
            AirInstData::Const(_)
            | AirInstData::BoolConst(_)
            | AirInstData::Param { .. }
            | AirInstData::Alloc { .. } => unreachable!(),
            // Ret and Call shouldn't appear in loop body/condition normally
            AirInstData::Ret(_) | AirInstData::Call { .. } => {}
            // Break and Continue have no dependencies to clear
            AirInstData::Break | AirInstData::Continue => {}
        }
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

        let mir = Lower::new(&air, 0, 0, "main").lower();

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

        let mir = Lower::new(&air, 0, 0, "main").lower();

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

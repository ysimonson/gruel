//! LLVM IR generation from Gruel CFG.
//!
//! This module is only compiled when the `llvm18` feature is enabled.

use std::collections::HashMap;

use gruel_air::{Type, TypeInternPool, TypeKind};
use gruel_cfg::{BlockId, Cfg, CfgInstData, CfgValue, Terminator};
use gruel_error::{CompileError, CompileResult, ErrorKind};
use inkwell::IntPredicate;
use inkwell::OptimizationLevel;
use inkwell::basic_block::BasicBlock as LlvmBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target as LlvmTarget, TargetMachine,
};
use inkwell::types::BasicType;
use inkwell::intrinsics::Intrinsic;
use inkwell::values::{BasicValueEnum, FunctionValue, PhiValue};
use lasso::ThreadedRodeo;

use crate::types::{gruel_type_to_llvm, gruel_type_to_llvm_param};

/// Convert an LLVM-related error string into a [`CompileError`].
fn llvm_error(msg: impl Into<String>) -> CompileError {
    CompileError::without_span(ErrorKind::InternalError(msg.into()))
}

/// Generate a native object file from a set of function CFGs.
///
/// All functions are lowered into a single LLVM module. The module is then
/// compiled to an in-memory object file buffer by the host machine's LLVM
/// code generator.
pub fn generate(
    functions: &[&Cfg],
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
) -> CompileResult<Vec<u8>> {
    // Initialize LLVM's native target (the machine we are running on).
    LlvmTarget::initialize_native(&InitializationConfig::default())
        .map_err(|e| llvm_error(format!("LLVM target initialization failed: {}", e)))?;

    let context = Context::create();
    let module = context.create_module("gruel_module");
    let builder = context.create_builder();

    // Declare all functions in the module first so that forward calls resolve.
    let mut declared: Vec<(&Cfg, FunctionValue<'_>)> = Vec::with_capacity(functions.len());
    for cfg in functions {
        let fn_value = declare_function(cfg, &context, &module, type_pool)?;
        declared.push((cfg, fn_value));
    }

    // Build a name → FunctionValue map for call resolution.
    let fn_map: HashMap<&str, FunctionValue<'_>> = declared
        .iter()
        .map(|(cfg, fv)| (cfg.fn_name(), *fv))
        .collect();

    // Define each function body.
    for (cfg, fn_value) in &declared {
        define_function(cfg, fn_value, &context, &builder, &module, type_pool, strings, interner, &fn_map)?;
    }

    // Verify the module before emitting.
    module.verify().map_err(|e| llvm_error(format!("LLVM module verification failed: {}", e)))?;

    // Set up a TargetMachine for the host.
    let target_triple = TargetMachine::get_default_triple();
    let llvm_target = LlvmTarget::from_triple(&target_triple)
        .map_err(|e| llvm_error(format!("failed to get LLVM target: {}", e)))?;
    let target_machine = llvm_target
        .create_target_machine(
            &target_triple,
            "generic",
            "",
            OptimizationLevel::None,
            RelocMode::Default,
            CodeModel::Default,
        )
        .ok_or_else(|| llvm_error("failed to create LLVM TargetMachine"))?;

    // Emit object code into an in-memory buffer.
    let obj_buf = target_machine
        .write_to_memory_buffer(&module, FileType::Object)
        .map_err(|e| llvm_error(format!("LLVM object emission failed: {}", e)))?;

    Ok(obj_buf.as_slice().to_vec())
}

/// Declare a Gruel function in the LLVM module (signature only).
///
/// The function body is filled in by [`define_function`]. Declaring all
/// functions before defining any allows mutual recursion to resolve.
fn declare_function<'ctx>(
    cfg: &Cfg,
    ctx: &'ctx Context,
    module: &Module<'ctx>,
    type_pool: &TypeInternPool,
) -> CompileResult<FunctionValue<'ctx>> {
    let name = cfg.fn_name();
    let param_types = collect_param_types(cfg, ctx, type_pool);

    let fn_type = match gruel_type_to_llvm(cfg.return_type(), ctx, type_pool) {
        Some(ret_ty) => ret_ty.fn_type(&param_types, false),
        None => ctx.void_type().fn_type(&param_types, false),
    };

    Ok(module.add_function(name, fn_type, None))
}

/// Collect LLVM parameter types for a function's parameter slots.
///
/// Uses the `param_type()` API on the CFG (which is populated from the AIR
/// before DCE runs) so that unused parameter types are not lost.
fn collect_param_types<'ctx>(
    cfg: &Cfg,
    ctx: &'ctx Context,
    type_pool: &TypeInternPool,
) -> Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> {
    let num_params = cfg.num_params() as usize;
    if num_params == 0 {
        return vec![];
    }

    (0..num_params)
        .filter_map(|i| {
            if cfg.is_param_inout(i as u32) {
                // By-reference params are always opaque pointers in LLVM IR.
                Some(ctx.ptr_type(inkwell::AddressSpace::default()).into())
            } else {
                // Value params: use the stored param type (survives DCE).
                let ty = cfg.param_type(i as u32)?;
                gruel_type_to_llvm_param(ty, ctx, type_pool)
            }
        })
        .collect()
}

/// State for translating one function.
struct FnCodegen<'ctx, 'a> {
    cfg: &'a Cfg,
    fn_value: FunctionValue<'ctx>,
    ctx: &'ctx Context,
    builder: &'a Builder<'ctx>,
    module: &'a Module<'ctx>,
    type_pool: &'a TypeInternPool,
    interner: &'a ThreadedRodeo,
    fn_map: &'a HashMap<&'a str, FunctionValue<'ctx>>,
    /// Maps CFG block IDs to LLVM basic blocks.
    llvm_blocks: Vec<LlvmBlock<'ctx>>,
    /// Maps CFG value indices to LLVM values.
    values: Vec<Option<BasicValueEnum<'ctx>>>,
    /// Alloca slots for local variables (one per slot index).
    locals: Vec<Option<inkwell::values::PointerValue<'ctx>>>,
    /// Phi nodes for block parameters (indexed by CfgValue index).
    /// Created before translation so that `Goto`/`Branch` terminators can
    /// add incoming edges even when the target block is processed later.
    phi_nodes: Vec<Option<PhiValue<'ctx>>>,
}

impl<'ctx, 'a> FnCodegen<'ctx, 'a> {
    fn new(
        cfg: &'a Cfg,
        fn_value: FunctionValue<'ctx>,
        ctx: &'ctx Context,
        builder: &'a Builder<'ctx>,
        module: &'a Module<'ctx>,
        type_pool: &'a TypeInternPool,
        _strings: &'a [String],
        interner: &'a ThreadedRodeo,
        fn_map: &'a HashMap<&'a str, FunctionValue<'ctx>>,
    ) -> Self {
        let value_count = cfg.value_count();
        let num_locals = cfg.num_locals() as usize;

        // Create LLVM basic blocks for each CFG block.
        let llvm_blocks: Vec<LlvmBlock<'ctx>> = cfg
            .blocks()
            .iter()
            .map(|bb| ctx.append_basic_block(fn_value, &format!("bb{}", bb.id.as_u32())))
            .collect();

        Self {
            cfg,
            fn_value,
            ctx,
            builder,
            module,
            type_pool,
            interner,
            fn_map,
            llvm_blocks,
            values: vec![None; value_count],
            locals: vec![None; num_locals],
            phi_nodes: vec![None; value_count],
        }
    }

    /// Get the LLVM block for a CFG block ID.
    fn llvm_block(&self, id: BlockId) -> LlvmBlock<'ctx> {
        self.llvm_blocks[id.as_u32() as usize]
    }

    /// Get a previously computed LLVM value.
    fn get_value(&self, v: CfgValue) -> BasicValueEnum<'ctx> {
        self.values[v.as_u32() as usize]
            .expect("CFG value not yet computed — likely a block ordering bug")
    }

    /// Store a computed LLVM value.
    fn set_value(&mut self, v: CfgValue, llvm_val: BasicValueEnum<'ctx>) {
        self.values[v.as_u32() as usize] = Some(llvm_val);
    }

    /// Get or create an alloca slot for a local variable.
    ///
    /// Allocas are created lazily in the function's entry block so that LLVM's
    /// `mem2reg` pass can promote them to SSA values without trouble.
    fn get_or_create_local(&mut self, slot: u32, ty: Type) -> inkwell::values::PointerValue<'ctx> {
        let slot = slot as usize;
        if let Some(ptr) = self.locals[slot] {
            return ptr;
        }

        // Insert alloca at the start of the entry block so mem2reg can see it.
        let entry_bb = self.llvm_blocks[0];
        let current_bb = self.builder.get_insert_block();

        // Position builder before the first instruction of the entry block.
        match entry_bb.get_first_instruction() {
            Some(first) => self.builder.position_before(&first),
            None => self.builder.position_at_end(entry_bb),
        }

        let llvm_ty = gruel_type_to_llvm(ty, self.ctx, self.type_pool)
            .expect("cannot alloca void-typed local");
        let ptr = self.builder.build_alloca(llvm_ty, &format!("slot{}", slot))
            .expect("build_alloca failed");

        // Restore builder position.
        if let Some(bb) = current_bb {
            self.builder.position_at_end(bb);
        }

        self.locals[slot] = Some(ptr);
        ptr
    }

    /// Get or declare an external `() -> !` C function (e.g. `__gruel_overflow`).
    fn get_or_declare_noreturn_fn(&self, name: &str) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function(name) {
            return f;
        }
        let fn_type = self.ctx.void_type().fn_type(&[], false);
        let f = self.module.add_function(name, fn_type, None);
        // Mark as `noreturn` so LLVM knows control never returns.
        f.add_attribute(
            inkwell::attributes::AttributeLoc::Function,
            self.ctx.create_string_attribute("noreturn", ""),
        );
        f
    }

    /// Emit an overflow-checked binary integer operation.
    ///
    /// Uses LLVM's `llvm.{sadd,ssub,smul,uadd,usub,umul}.with.overflow` intrinsics.
    /// On overflow, calls `__gruel_overflow()` (which exits with code 101).
    ///
    /// `intrinsic_name` should be e.g. `"llvm.sadd.with.overflow"`.
    fn build_checked_int_op(
        &mut self,
        l: inkwell::values::IntValue<'ctx>,
        r: inkwell::values::IntValue<'ctx>,
        intrinsic_name: &str,
    ) -> inkwell::values::IntValue<'ctx> {
        let int_type = l.get_type();
        let intrinsic = Intrinsic::find(intrinsic_name)
            .unwrap_or_else(|| panic!("LLVM intrinsic '{}' not found", intrinsic_name));
        let intrinsic_fn = intrinsic
            .get_declaration(self.module, &[int_type.into()])
            .unwrap_or_else(|| panic!("failed to declare intrinsic '{}'", intrinsic_name));

        let call = self.builder.build_call(intrinsic_fn, &[l.into(), r.into()], "ovf").unwrap();
        let struct_val = call.try_as_basic_value().basic().unwrap().into_struct_value();
        let result = self.builder.build_extract_value(struct_val, 0, "res").unwrap().into_int_value();
        let overflow = self.builder.build_extract_value(struct_val, 1, "ovf_flag").unwrap().into_int_value();

        // Emit conditional branch to overflow handler or continuation.
        let current_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let overflow_bb = self.ctx.append_basic_block(current_fn, "ovf_handler");
        let cont_bb = self.ctx.append_basic_block(current_fn, "ovf_cont");

        self.builder.build_conditional_branch(overflow, overflow_bb, cont_bb).unwrap();

        // Overflow handler: call __gruel_overflow() then unreachable.
        self.builder.position_at_end(overflow_bb);
        let panic_fn = self.get_or_declare_noreturn_fn("__gruel_overflow");
        self.builder.build_call(panic_fn, &[], "").unwrap();
        self.builder.build_unreachable().unwrap();

        // Continue in the continuation block.
        self.builder.position_at_end(cont_bb);
        result
    }

    /// Emit a division-by-zero check.
    ///
    /// If `divisor` is zero, calls `__gruel_div_by_zero()` (exits with code 101).
    fn build_div_zero_check(&mut self, divisor: inkwell::values::IntValue<'ctx>) {
        let zero = divisor.get_type().const_zero();
        let is_zero = self.builder
            .build_int_compare(IntPredicate::EQ, divisor, zero, "divzero_check")
            .unwrap();

        let current_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let zero_bb = self.ctx.append_basic_block(current_fn, "divzero_handler");
        let cont_bb = self.ctx.append_basic_block(current_fn, "divzero_cont");

        self.builder.build_conditional_branch(is_zero, zero_bb, cont_bb).unwrap();

        // Div-by-zero handler.
        self.builder.position_at_end(zero_bb);
        let panic_fn = self.get_or_declare_noreturn_fn("__gruel_div_by_zero");
        self.builder.build_call(panic_fn, &[], "").unwrap();
        self.builder.build_unreachable().unwrap();

        // Continue.
        self.builder.position_at_end(cont_bb);
    }

    /// Pre-create LLVM phi nodes for all block parameters.
    ///
    /// This must run before any block translation so that `Goto`/`Branch`
    /// terminators can add incoming edges to phi nodes in forward-referenced
    /// blocks.
    fn create_phi_nodes(&mut self) {
        for block in self.cfg.blocks() {
            if block.params.is_empty() {
                continue;
            }
            let llvm_bb = self.llvm_block(block.id);
            self.builder.position_at_end(llvm_bb);

            for &(param_val, param_ty) in &block.params {
                let llvm_ty = gruel_type_to_llvm(param_ty, self.ctx, self.type_pool)
                    .expect("block param must have non-void type");
                let phi = self.builder
                    .build_phi(llvm_ty, &format!("p{}", param_val.as_u32()))
                    .expect("build_phi failed");
                let idx = param_val.as_u32() as usize;
                self.phi_nodes[idx] = Some(phi);
                // Pre-populate values so instructions referencing the param can
                // find it without knowing it is a phi node.
                self.values[idx] = Some(phi.as_basic_value());
            }
        }
    }

    /// Translate all blocks in the function.
    fn translate(&mut self) -> CompileResult<()> {
        self.create_phi_nodes();
        for block_idx in 0..self.cfg.blocks().len() {
            self.translate_block(BlockId::from_raw(block_idx as u32))?;
        }
        Ok(())
    }

    /// Translate one CFG block into its LLVM basic block.
    fn translate_block(&mut self, id: BlockId) -> CompileResult<()> {
        let llvm_bb = self.llvm_block(id);
        self.builder.position_at_end(llvm_bb);

        let block = self.cfg.get_block(id);
        let insts: Vec<CfgValue> = block.insts.clone();
        let term = block.terminator;

        for val in insts {
            self.translate_inst(val)?;
        }
        self.translate_terminator(term)?;

        Ok(())
    }

    /// Translate a single CFG instruction.
    fn translate_inst(&mut self, val: CfgValue) -> CompileResult<()> {
        let inst = self.cfg.get_inst(val).clone();
        let ty = inst.ty;

        let result: Option<BasicValueEnum<'ctx>> = match inst.data {
            CfgInstData::Const(n) => {
                // Unit-typed constants (e.g. `()`) have no LLVM representation.
                gruel_type_to_llvm(ty, self.ctx, self.type_pool)
                    .map(|llvm_ty| llvm_ty.into_int_type().const_int(n, true).into())
            }

            CfgInstData::BoolConst(b) => {
                Some(self.ctx.bool_type().const_int(b as u64, false).into())
            }

            CfgInstData::StringConst(_idx) => {
                // String constants: emit a null pointer placeholder.
                // Phase 2e will wire up the actual string table.
                Some(self.ctx.ptr_type(inkwell::AddressSpace::default()).const_null().into())
            }

            CfgInstData::Param { index } => {
                let param_val = self.fn_value.get_nth_param(index)
                    .expect("param index out of range");
                if self.cfg.is_param_inout(index) {
                    // By-ref param (inout or borrow): the LLVM arg is a pointer;
                    // load the value from it.
                    let llvm_ty = gruel_type_to_llvm(ty, self.ctx, self.type_pool)
                        .expect("by-ref param must have non-void type");
                    let ptr = param_val.into_pointer_value();
                    Some(self.builder.build_load(llvm_ty, ptr, "paramld").unwrap())
                } else {
                    Some(param_val)
                }
            }

            CfgInstData::BlockParam { .. } => {
                // Already materialized as a phi node in create_phi_nodes().
                // The phi value is pre-stored in self.values; return early to
                // avoid overwriting it with None.
                return Ok(());
            }

            // ---- Binary arithmetic ----
            CfgInstData::Add(lhs, rhs) => {
                let lhs_ty = self.cfg.get_inst(lhs).ty;
                let l = self.get_value(lhs).into_int_value();
                let r = self.get_value(rhs).into_int_value();
                let intrinsic = if is_signed_type(lhs_ty) {
                    "llvm.sadd.with.overflow"
                } else {
                    "llvm.uadd.with.overflow"
                };
                Some(self.build_checked_int_op(l, r, intrinsic).into())
            }
            CfgInstData::Sub(lhs, rhs) => {
                let lhs_ty = self.cfg.get_inst(lhs).ty;
                let l = self.get_value(lhs).into_int_value();
                let r = self.get_value(rhs).into_int_value();
                let intrinsic = if is_signed_type(lhs_ty) {
                    "llvm.ssub.with.overflow"
                } else {
                    "llvm.usub.with.overflow"
                };
                Some(self.build_checked_int_op(l, r, intrinsic).into())
            }
            CfgInstData::Mul(lhs, rhs) => {
                let lhs_ty = self.cfg.get_inst(lhs).ty;
                let l = self.get_value(lhs).into_int_value();
                let r = self.get_value(rhs).into_int_value();
                let intrinsic = if is_signed_type(lhs_ty) {
                    "llvm.smul.with.overflow"
                } else {
                    "llvm.umul.with.overflow"
                };
                Some(self.build_checked_int_op(l, r, intrinsic).into())
            }
            CfgInstData::Div(lhs, rhs) => {
                let l = self.get_value(lhs).into_int_value();
                let r = self.get_value(rhs).into_int_value();
                let lhs_ty = self.cfg.get_inst(lhs).ty;
                self.build_div_zero_check(r);
                let v = if is_signed_type(lhs_ty) {
                    self.builder.build_int_signed_div(l, r, "div").unwrap()
                } else {
                    self.builder.build_int_unsigned_div(l, r, "div").unwrap()
                };
                Some(v.into())
            }
            CfgInstData::Mod(lhs, rhs) => {
                let l = self.get_value(lhs).into_int_value();
                let r = self.get_value(rhs).into_int_value();
                let lhs_ty = self.cfg.get_inst(lhs).ty;
                self.build_div_zero_check(r);
                let v = if is_signed_type(lhs_ty) {
                    self.builder.build_int_signed_rem(l, r, "rem").unwrap()
                } else {
                    self.builder.build_int_unsigned_rem(l, r, "rem").unwrap()
                };
                Some(v.into())
            }

            // ---- Comparisons (produce i1) ----
            CfgInstData::Eq(lhs, rhs) => {
                let lhs_ty = self.cfg.get_inst(lhs).ty;
                if gruel_type_to_llvm(lhs_ty, self.ctx, self.type_pool).is_none() {
                    // Unit == Unit is always true.
                    Some(self.ctx.bool_type().const_int(1, false).into())
                } else {
                    let l = self.get_value(lhs).into_int_value();
                    let r = self.get_value(rhs).into_int_value();
                    Some(self.builder.build_int_compare(IntPredicate::EQ, l, r, "eq").unwrap().into())
                }
            }
            CfgInstData::Ne(lhs, rhs) => {
                let lhs_ty = self.cfg.get_inst(lhs).ty;
                if gruel_type_to_llvm(lhs_ty, self.ctx, self.type_pool).is_none() {
                    // Unit != Unit is always false.
                    Some(self.ctx.bool_type().const_int(0, false).into())
                } else {
                    let l = self.get_value(lhs).into_int_value();
                    let r = self.get_value(rhs).into_int_value();
                    Some(self.builder.build_int_compare(IntPredicate::NE, l, r, "ne").unwrap().into())
                }
            }
            CfgInstData::Lt(lhs, rhs) => {
                let lhs_ty = self.cfg.get_inst(lhs).ty;
                let (pred, upred) = (IntPredicate::SLT, IntPredicate::ULT);
                let l = self.get_value(lhs).into_int_value();
                let r = self.get_value(rhs).into_int_value();
                let p = if is_signed_type(lhs_ty) { pred } else { upred };
                Some(self.builder.build_int_compare(p, l, r, "lt").unwrap().into())
            }
            CfgInstData::Gt(lhs, rhs) => {
                let lhs_ty = self.cfg.get_inst(lhs).ty;
                let (pred, upred) = (IntPredicate::SGT, IntPredicate::UGT);
                let l = self.get_value(lhs).into_int_value();
                let r = self.get_value(rhs).into_int_value();
                let p = if is_signed_type(lhs_ty) { pred } else { upred };
                Some(self.builder.build_int_compare(p, l, r, "gt").unwrap().into())
            }
            CfgInstData::Le(lhs, rhs) => {
                let lhs_ty = self.cfg.get_inst(lhs).ty;
                let (pred, upred) = (IntPredicate::SLE, IntPredicate::ULE);
                let l = self.get_value(lhs).into_int_value();
                let r = self.get_value(rhs).into_int_value();
                let p = if is_signed_type(lhs_ty) { pred } else { upred };
                Some(self.builder.build_int_compare(p, l, r, "le").unwrap().into())
            }
            CfgInstData::Ge(lhs, rhs) => {
                let lhs_ty = self.cfg.get_inst(lhs).ty;
                let (pred, upred) = (IntPredicate::SGE, IntPredicate::UGE);
                let l = self.get_value(lhs).into_int_value();
                let r = self.get_value(rhs).into_int_value();
                let p = if is_signed_type(lhs_ty) { pred } else { upred };
                Some(self.builder.build_int_compare(p, l, r, "ge").unwrap().into())
            }

            // ---- Bitwise ----
            CfgInstData::BitAnd(lhs, rhs) => {
                let l = self.get_value(lhs).into_int_value();
                let r = self.get_value(rhs).into_int_value();
                Some(self.builder.build_and(l, r, "and").unwrap().into())
            }
            CfgInstData::BitOr(lhs, rhs) => {
                let l = self.get_value(lhs).into_int_value();
                let r = self.get_value(rhs).into_int_value();
                Some(self.builder.build_or(l, r, "or").unwrap().into())
            }
            CfgInstData::BitXor(lhs, rhs) => {
                let l = self.get_value(lhs).into_int_value();
                let r = self.get_value(rhs).into_int_value();
                Some(self.builder.build_xor(l, r, "xor").unwrap().into())
            }
            CfgInstData::Shl(lhs, rhs) => {
                let l = self.get_value(lhs).into_int_value();
                let r = self.get_value(rhs).into_int_value();
                Some(self.builder.build_left_shift(l, r, "shl").unwrap().into())
            }
            CfgInstData::Shr(lhs, rhs) => {
                let lhs_ty = self.cfg.get_inst(lhs).ty;
                let l = self.get_value(lhs).into_int_value();
                let r = self.get_value(rhs).into_int_value();
                // Arithmetic right shift for signed types; logical for unsigned.
                let signed = is_signed_type(lhs_ty);
                Some(self.builder.build_right_shift(l, r, signed, "shr").unwrap().into())
            }

            // ---- Unary ----
            CfgInstData::Neg(operand) => {
                let v = self.get_value(operand).into_int_value();
                // Negation on signed types: 0 - v, with overflow check.
                let zero = v.get_type().const_zero();
                Some(self.build_checked_int_op(zero, v, "llvm.ssub.with.overflow").into())
            }
            CfgInstData::Not(operand) => {
                // Boolean not: compare == 0.
                let v = self.get_value(operand).into_int_value();
                let zero = v.get_type().const_zero();
                Some(self.builder.build_int_compare(IntPredicate::EQ, v, zero, "not").unwrap().into())
            }
            CfgInstData::BitNot(operand) => {
                let v = self.get_value(operand).into_int_value();
                Some(self.builder.build_not(v, "bitnot").unwrap().into())
            }

            // ---- Local variable operations ----
            CfgInstData::Alloc { slot, init } => {
                let init_ty = self.cfg.get_inst(init).ty;
                // Unit-typed locals have no LLVM representation — skip.
                if gruel_type_to_llvm(init_ty, self.ctx, self.type_pool).is_some() {
                    let init_val = self.get_value(init);
                    let ptr = self.get_or_create_local(slot, init_ty);
                    self.builder.build_store(ptr, init_val).unwrap();
                }
                None
            }
            CfgInstData::Load { slot } => {
                match gruel_type_to_llvm(ty, self.ctx, self.type_pool) {
                    None => None, // Unit-typed load — no LLVM value.
                    Some(llvm_ty) => {
                        let ptr = self.locals[slot as usize]
                            .expect("Load before Alloc — invalid CFG");
                        Some(self.builder.build_load(llvm_ty, ptr, "load").unwrap())
                    }
                }
            }
            CfgInstData::Store { slot, value } => {
                let value_ty = self.cfg.get_inst(value).ty;
                // Unit-typed stores have no LLVM representation — skip.
                if gruel_type_to_llvm(value_ty, self.ctx, self.type_pool).is_some() {
                    let v = self.get_value(value);
                    let ptr = self.get_or_create_local(slot, value_ty);
                    self.builder.build_store(ptr, v).unwrap();
                }
                None
            }
            CfgInstData::ParamStore { param_slot, value } => {
                let v = self.get_value(value);
                let ptr_val = self.fn_value.get_nth_param(param_slot)
                    .expect("param_slot out of range")
                    .into_pointer_value();
                self.builder.build_store(ptr_val, v).unwrap();
                None
            }

            // ---- Function calls ----
            CfgInstData::Call { name, args_start, args_len } => {
                let fn_name = self.interner.resolve(&name);
                // Look up in the declared-functions map, then fall back to the module
                // (handles calls to external/runtime functions if they were declared).
                let callee = self.fn_map.get(fn_name).copied()
                    .or_else(|| self.module.get_function(fn_name))
                    .ok_or_else(|| llvm_error(format!("undefined function '{}'", fn_name)))?;

                let args = self.cfg.get_call_args(args_start, args_len).to_vec();
                let call_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = args
                    .iter()
                    .map(|arg| {
                        if arg.is_inout() || arg.is_borrow() {
                            // By-ref: pass the alloca pointer, not the loaded value.
                            // The arg value should be produced by a Load { slot }.
                            let inst = self.cfg.get_inst(arg.value);
                            if let CfgInstData::Load { slot } = inst.data {
                                let ptr = self.locals[slot as usize]
                                    .expect("inout/borrow arg: slot not yet allocated");
                                return inkwell::values::BasicMetadataValueEnum::from(ptr);
                            }
                            // Fallback for unexpected shapes — pass the value as-is.
                        }
                        self.get_value(arg.value).into()
                    })
                    .collect();

                let call_site = self.builder.build_call(callee, &call_args, "call").unwrap();
                // `basic()` returns Some for non-void calls, None for void.
                call_site.try_as_basic_value().basic()
            }

            CfgInstData::Intrinsic { name, args_start, args_len } => {
                let name_str = self.interner.resolve(&name);
                let args = self.cfg.get_extra(args_start, args_len).to_vec();
                self.translate_intrinsic(ty, name_str, &args)
            }

            // ---- Integer cast ----
            CfgInstData::IntCast { value, from_ty } => {
                let v = self.get_value(value).into_int_value();
                let dst_ty = gruel_type_to_llvm(ty, self.ctx, self.type_pool)
                    .expect("IntCast target must be non-void")
                    .into_int_type();
                let src_bits = v.get_type().get_bit_width();
                let dst_bits = dst_ty.get_bit_width();
                let result = if dst_bits > src_bits {
                    // Widening — never overflows.
                    if is_signed_type(from_ty) {
                        self.builder.build_int_s_extend(v, dst_ty, "sext").unwrap()
                    } else {
                        self.builder.build_int_z_extend(v, dst_ty, "zext").unwrap()
                    }
                } else if dst_bits < src_bits {
                    // Narrowing — check that the value fits in the smaller type.
                    let truncated = self.builder.build_int_truncate(v, dst_ty, "trunc").unwrap();
                    let src_ty = v.get_type();
                    // Round-trip: extend truncated value back to source width.
                    let extended = if is_signed_type(ty) {
                        self.builder.build_int_s_extend(truncated, src_ty, "sext_chk").unwrap()
                    } else {
                        self.builder.build_int_z_extend(truncated, src_ty, "zext_chk").unwrap()
                    };
                    // If extended != original, the value doesn't fit.
                    let fits = self.builder
                        .build_int_compare(IntPredicate::EQ, v, extended, "fits")
                        .unwrap();
                    // Emit conditional branch to intcast overflow handler.
                    let current_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                    let overflow_bb = self.ctx.append_basic_block(current_fn, "icast_ovf");
                    let cont_bb = self.ctx.append_basic_block(current_fn, "icast_cont");
                    self.builder.build_conditional_branch(fits, cont_bb, overflow_bb).unwrap();
                    // Overflow handler.
                    self.builder.position_at_end(overflow_bb);
                    let panic_fn = self.get_or_declare_noreturn_fn("__gruel_intcast_overflow");
                    self.builder.build_call(panic_fn, &[], "").unwrap();
                    self.builder.build_unreachable().unwrap();
                    // Continue.
                    self.builder.position_at_end(cont_bb);
                    truncated
                } else {
                    v // Same width — no-op.
                };
                Some(result.into())
            }

            // ---- Drop / storage liveness — no-ops in LLVM backend ----
            CfgInstData::Drop { .. }
            | CfgInstData::StorageLive { .. }
            | CfgInstData::StorageDead { .. } => None,

            // ---- Composite ops — Phase 2d ----
            CfgInstData::PlaceRead { .. }
            | CfgInstData::PlaceWrite { .. }
            | CfgInstData::StructInit { .. }
            | CfgInstData::FieldSet { .. }
            | CfgInstData::ParamFieldSet { .. }
            | CfgInstData::ArrayInit { .. }
            | CfgInstData::IndexSet { .. }
            | CfgInstData::ParamIndexSet { .. }
            | CfgInstData::EnumVariant { .. } => {
                // Not yet implemented — emit zero placeholder.
                gruel_type_to_llvm(ty, self.ctx, self.type_pool).map(|t| t.const_zero())
            }
        };

        if let Some(v) = result {
            self.set_value(val, v);
        }
        Ok(())
    }

    /// Translate a CFG intrinsic call into LLVM IR.
    ///
    /// Returns the result value (or `None` for unit-returning intrinsics like `@dbg`).
    fn translate_intrinsic(
        &mut self,
        ty: Type,
        name_str: &str,
        args: &[CfgValue],
    ) -> Option<BasicValueEnum<'ctx>> {
        match name_str {
            // ---- Random number generation ----
            "random_u32" => {
                let fn_ty = self.ctx.i32_type().fn_type(&[], false);
                let f = self.module.get_function("__gruel_random_u32")
                    .unwrap_or_else(|| self.module.add_function("__gruel_random_u32", fn_ty, None));
                self.builder.build_call(f, &[], "rand").unwrap().try_as_basic_value().basic()
            }
            "random_u64" => {
                let fn_ty = self.ctx.i64_type().fn_type(&[], false);
                let f = self.module.get_function("__gruel_random_u64")
                    .unwrap_or_else(|| self.module.add_function("__gruel_random_u64", fn_ty, None));
                self.builder.build_call(f, &[], "rand").unwrap().try_as_basic_value().basic()
            }

            // ---- Debug print ----
            "dbg" => {
                if !args.is_empty() {
                    let arg_val = args[0];
                    let arg_ty = self.cfg.get_inst(arg_val).ty;
                    let i64_ty = self.ctx.i64_type();
                    match arg_ty.kind() {
                        TypeKind::I8 | TypeKind::I16 | TypeKind::I32 | TypeKind::I64 => {
                            let v = self.get_value(arg_val).into_int_value();
                            let v64 = if v.get_type().get_bit_width() < 64 {
                                self.builder.build_int_s_extend(v, i64_ty, "sext").unwrap()
                            } else {
                                v
                            };
                            let fn_ty = self.ctx.void_type().fn_type(&[i64_ty.into()], false);
                            let f = self.module.get_function("__gruel_dbg_i64")
                                .unwrap_or_else(|| self.module.add_function("__gruel_dbg_i64", fn_ty, None));
                            self.builder.build_call(f, &[v64.into()], "").unwrap();
                        }
                        TypeKind::U8 | TypeKind::U16 | TypeKind::U32 | TypeKind::U64 => {
                            let v = self.get_value(arg_val).into_int_value();
                            let v64 = if v.get_type().get_bit_width() < 64 {
                                self.builder.build_int_z_extend(v, i64_ty, "zext").unwrap()
                            } else {
                                v
                            };
                            let fn_ty = self.ctx.void_type().fn_type(&[i64_ty.into()], false);
                            let f = self.module.get_function("__gruel_dbg_u64")
                                .unwrap_or_else(|| self.module.add_function("__gruel_dbg_u64", fn_ty, None));
                            self.builder.build_call(f, &[v64.into()], "").unwrap();
                        }
                        TypeKind::Bool => {
                            let v = self.get_value(arg_val).into_int_value();
                            let v64 = self.builder.build_int_z_extend(v, i64_ty, "zext").unwrap();
                            let fn_ty = self.ctx.void_type().fn_type(&[i64_ty.into()], false);
                            let f = self.module.get_function("__gruel_dbg_bool")
                                .unwrap_or_else(|| self.module.add_function("__gruel_dbg_bool", fn_ty, None));
                            self.builder.build_call(f, &[v64.into()], "").unwrap();
                        }
                        _ => {
                            // String and complex types — handled in Phase 2d.
                        }
                    }
                }
                None // @dbg always returns unit
            }

            // ---- Pointer operations ----
            "ptr_read" => {
                let ptr_val = args[0];
                let ptr = self.get_value(ptr_val).into_pointer_value();
                let result_llvm_ty = gruel_type_to_llvm(ty, self.ctx, self.type_pool)
                    .expect("ptr_read must return a non-void type");
                Some(self.builder.build_load(result_llvm_ty, ptr, "ptrrd").unwrap())
            }
            "ptr_write" => {
                let ptr_val = args[0];
                let written_val = args[1];
                let ptr = self.get_value(ptr_val).into_pointer_value();
                let v = self.get_value(written_val);
                self.builder.build_store(ptr, v).unwrap();
                None
            }
            "ptr_offset" => {
                let ptr_val = args[0];
                let offset_val = args[1];
                let ptr = self.get_value(ptr_val).into_pointer_value();
                let offset = self.get_value(offset_val).into_int_value();
                // Determine the pointee type so GEP can compute the stride.
                let ptr_ty = self.cfg.get_inst(ptr_val).ty;
                let pointee_ty = match ptr_ty.kind() {
                    TypeKind::PtrConst(id) => self.type_pool.ptr_const_def(id),
                    TypeKind::PtrMut(id) => self.type_pool.ptr_mut_def(id),
                    _ => return Some(ptr.into()), // not actually a pointer — no-op
                };
                let result_ptr = if let Some(elem_llvm) = gruel_type_to_llvm(pointee_ty, self.ctx, self.type_pool) {
                    // GEP advances by `offset * sizeof(elem)` automatically.
                    unsafe { self.builder.build_gep(elem_llvm, ptr, &[offset], "gep").unwrap() }
                } else {
                    ptr // zero-sized pointee — offset has no effect
                };
                Some(result_ptr.into())
            }
            "ptr_to_int" => {
                let ptr_val = args[0];
                let ptr = self.get_value(ptr_val).into_pointer_value();
                let i64_ty = self.ctx.i64_type();
                Some(self.builder.build_ptr_to_int(ptr, i64_ty, "p2i").unwrap().into())
            }
            "int_to_ptr" => {
                let addr_val = args[0];
                let addr = self.get_value(addr_val).into_int_value();
                let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
                Some(self.builder.build_int_to_ptr(addr, ptr_ty, "i2p").unwrap().into())
            }

            // ---- Address-of (raw pointer to local) ----
            "raw" | "raw_mut" => {
                let lvalue_val = args[0];
                let lvalue_inst = self.cfg.get_inst(lvalue_val).clone();
                if let CfgInstData::Load { slot } = lvalue_inst.data {
                    let slot_ty = lvalue_inst.ty;
                    let ptr = self.get_or_create_local(slot, slot_ty);
                    Some(ptr.into())
                } else {
                    // Fallback for non-local lvalues (Phase 2d will handle place exprs).
                    gruel_type_to_llvm(ty, self.ctx, self.type_pool).map(|t| t.const_zero())
                }
            }

            // ---- Unimplemented (String intrinsics need Phase 2d struct support) ----
            _ => {
                gruel_type_to_llvm(ty, self.ctx, self.type_pool).map(|t| t.const_zero())
            }
        }
    }

    /// Translate a CFG terminator into LLVM control flow.
    fn translate_terminator(&mut self, term: Terminator) -> CompileResult<()> {
        match term {
            Terminator::Return { value: Some(v) } => {
                let ty = self.cfg.get_inst(v).ty;
                if gruel_type_to_llvm(ty, self.ctx, self.type_pool).is_none() {
                    // Unit-typed return value → void return.
                    self.builder.build_return(None).unwrap();
                } else {
                    let ret_val = self.get_value(v);
                    self.builder.build_return(Some(&ret_val)).unwrap();
                }
            }
            Terminator::Return { value: None } => {
                self.builder.build_return(None).unwrap();
            }
            Terminator::Goto { target, args_start, args_len } => {
                let current_bb = self.builder.get_insert_block().unwrap();
                // Wire up phi incoming values for the target block's parameters.
                let args = self.cfg.get_extra(args_start, args_len).to_vec();
                let params = self.cfg.get_block(target).params.clone();
                for (i, (param_val, _)) in params.iter().enumerate() {
                    let incoming = self.get_value(args[i]);
                    let phi = self.phi_nodes[param_val.as_u32() as usize]
                        .expect("phi node missing for block param");
                    phi.add_incoming(&[(&incoming, current_bb)]);
                }
                let target_bb = self.llvm_block(target);
                self.builder.build_unconditional_branch(target_bb).unwrap();
            }
            Terminator::Branch {
                cond,
                then_block, then_args_start, then_args_len,
                else_block, else_args_start, else_args_len,
            } => {
                let current_bb = self.builder.get_insert_block().unwrap();
                let cond_val = self.get_value(cond).into_int_value();
                let cond_i1 = if cond_val.get_type().get_bit_width() == 1 {
                    cond_val
                } else {
                    let zero = cond_val.get_type().const_zero();
                    self.builder.build_int_compare(IntPredicate::NE, cond_val, zero, "cond").unwrap()
                };
                // Wire up phi incoming values for then-branch params.
                let then_args = self.cfg.get_extra(then_args_start, then_args_len).to_vec();
                let then_params = self.cfg.get_block(then_block).params.clone();
                for (i, (param_val, _)) in then_params.iter().enumerate() {
                    let incoming = self.get_value(then_args[i]);
                    let phi = self.phi_nodes[param_val.as_u32() as usize]
                        .expect("phi node missing for then-block param");
                    phi.add_incoming(&[(&incoming, current_bb)]);
                }
                // Wire up phi incoming values for else-branch params.
                let else_args = self.cfg.get_extra(else_args_start, else_args_len).to_vec();
                let else_params = self.cfg.get_block(else_block).params.clone();
                for (i, (param_val, _)) in else_params.iter().enumerate() {
                    let incoming = self.get_value(else_args[i]);
                    let phi = self.phi_nodes[param_val.as_u32() as usize]
                        .expect("phi node missing for else-block param");
                    phi.add_incoming(&[(&incoming, current_bb)]);
                }
                let then_bb = self.llvm_block(then_block);
                let else_bb = self.llvm_block(else_block);
                self.builder.build_conditional_branch(cond_i1, then_bb, else_bb).unwrap();
            }
            Terminator::Switch { scrutinee, cases_start, cases_len, default } => {
                let val = self.get_value(scrutinee).into_int_value();
                let default_bb = self.llvm_block(default);
                let cases = self.cfg.get_switch_cases(cases_start, cases_len);
                // Deduplicate case values: LLVM forbids duplicate case values.
                // Keep only the first occurrence (same behavior as native backend).
                let mut seen = std::collections::HashSet::new();
                let llvm_cases: Vec<_> = cases
                    .iter()
                    .filter(|(case_val, _)| seen.insert(*case_val))
                    .map(|(case_val, case_block)| {
                        let case_int = val.get_type().const_int(*case_val as u64, true);
                        (case_int, self.llvm_block(*case_block))
                    })
                    .collect();
                self.builder.build_switch(val, default_bb, &llvm_cases).unwrap();
            }
            Terminator::Unreachable | Terminator::None => {
                self.builder.build_unreachable().unwrap();
            }
        }
        Ok(())
    }
}

/// Generate the body of a declared LLVM function from its CFG.
fn define_function<'ctx>(
    cfg: &Cfg,
    fn_value: &FunctionValue<'ctx>,
    ctx: &'ctx Context,
    builder: &Builder<'ctx>,
    module: &Module<'ctx>,
    type_pool: &TypeInternPool,
    strings: &[String],
    interner: &ThreadedRodeo,
    fn_map: &HashMap<&str, FunctionValue<'ctx>>,
) -> CompileResult<()> {
    let mut fn_gen = FnCodegen::new(cfg, *fn_value, ctx, builder, module, type_pool, strings, interner, fn_map);
    fn_gen.translate()
}

/// Returns true for signed integer types.
fn is_signed_type(ty: Type) -> bool {
    matches!(
        ty.kind(),
        TypeKind::I8 | TypeKind::I16 | TypeKind::I32 | TypeKind::I64
    )
}

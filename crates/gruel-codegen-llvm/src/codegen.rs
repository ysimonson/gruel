//! LLVM IR generation from Gruel CFG.
//!
//! This module is only compiled when the `llvm18` feature is enabled.

use std::collections::HashMap;

use gruel_air::{StructId, Type, TypeInternPool, TypeKind};
use gruel_cfg::{BlockId, Cfg, CfgInstData, CfgValue, PlaceBase, Projection, Terminator};
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
use inkwell::values::{AggregateValueEnum, BasicValue, BasicValueEnum, FunctionValue, GlobalValue, PhiValue};
use lasso::ThreadedRodeo;

use crate::types::{abi_slot_count, gruel_type_to_llvm, gruel_type_to_llvm_param};

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

    // Create LLVM global constants for each string literal.
    // Each global holds the raw UTF-8 bytes. The String struct's `ptr` field
    // points to the start of this data.
    let string_globals: Vec<GlobalValue<'_>> = strings
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let bytes = s.as_bytes();
            // Use const_string (no null terminator needed — we have explicit len)
            let array_val = context.const_string(bytes, false);
            let global = module.add_global(
                context.i8_type().array_type(bytes.len() as u32),
                None,
                &format!(".str.{}", i),
            );
            global.set_constant(true);
            global.set_linkage(inkwell::module::Linkage::Private);
            global.set_initializer(&array_val);
            global
        })
        .collect();

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
        define_function(cfg, fn_value, &context, &builder, &module, type_pool, strings, &string_globals, interner, &fn_map)?;
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

/// Collect LLVM parameter types for a Gruel function.
///
/// Gruel uses a flattened native ABI where composite types (structs, arrays)
/// occupy multiple ABI slots. In LLVM IR we use aggregate types directly, so
/// each Gruel parameter maps to exactly one LLVM parameter (not one per slot).
///
/// The function advances by `abi_slot_count(ty)` per Gruel parameter so that
/// intermediate slots of multi-slot params are skipped.
fn collect_param_types<'ctx>(
    cfg: &Cfg,
    ctx: &'ctx Context,
    type_pool: &TypeInternPool,
) -> Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> {
    let num_params = cfg.num_params() as usize;
    if num_params == 0 {
        return vec![];
    }

    let mut result = Vec::new();
    let mut i = 0usize;
    while i < num_params {
        if cfg.is_param_inout(i as u32) {
            // By-reference params are always opaque pointers in LLVM IR (1 slot).
            result.push(ctx.ptr_type(inkwell::AddressSpace::default()).into());
            i += 1;
        } else {
            // Value params: emit one LLVM param per Gruel param, skipping
            // the intermediate ABI slots of multi-slot composites.
            let ty = cfg.param_type(i as u32)
                .expect("param slot in range must have a type");
            let raw_slot_count = abi_slot_count(ty, type_pool);
            if raw_slot_count > 0 {
                // Non-zero ABI slot count → type has an LLVM representation.
                if let Some(llvm_ty) = gruel_type_to_llvm_param(ty, ctx, type_pool) {
                    result.push(llvm_ty);
                }
            }
            // Advance past all ABI slots for this param (at least 1 to avoid loops).
            i += raw_slot_count.max(1) as usize;
        }
    }
    result
}

/// Build the mapping from ABI slot index to LLVM parameter index.
///
/// Because composite types occupy multiple ABI slots but map to a single LLVM
/// parameter, we need this table to translate `Param { index: abi_slot }` CFG
/// instructions into the correct `get_nth_param(llvm_idx)` call.
///
/// Zero-sized types (unit, never, all-void structs) have `abi_slot_count = 0`
/// and do not consume any ABI slots, so they are never in the slot table.
/// Non-zero-slot types always have a non-void LLVM type, so `abi_slot_count > 0`
/// reliably predicts whether an LLVM param is emitted.
fn build_slot_to_llvm_param(cfg: &Cfg, type_pool: &TypeInternPool) -> Vec<u32> {
    let num_params = cfg.num_params() as usize;
    let mut table = vec![0u32; num_params];
    let mut llvm_idx: u32 = 0;
    let mut i = 0usize;
    while i < num_params {
        if cfg.is_param_inout(i as u32) {
            table[i] = llvm_idx;
            llvm_idx += 1;
            i += 1;
        } else {
            let ty = cfg.param_type(i as u32)
                .expect("param slot in range must have a type");
            let raw_slot_count = abi_slot_count(ty, type_pool);
            let slot_count = raw_slot_count.max(1) as usize;
            // All ABI slots of this Gruel param share the same LLVM param index.
            for k in 0..slot_count {
                if i + k < num_params {
                    table[i + k] = llvm_idx;
                }
            }
            // Advance llvm_idx only when the type has an LLVM representation,
            // i.e. when abi_slot_count > 0 (zero-sized types have no LLVM param).
            if raw_slot_count > 0 {
                llvm_idx += 1;
            }
            i += slot_count;
        }
    }
    table
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
    /// Module-level string literal data (raw Rust strings).
    strings: &'a [String],
    /// LLVM globals holding the raw bytes of each string literal.
    string_globals: &'a [GlobalValue<'ctx>],
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
    /// Alloca slots for value parameters that need GEP access (field/index).
    /// Non-inout params are passed by value in LLVM; when field/index access
    /// is needed, we spill them to an alloca so GEP can address into them.
    param_allocas: Vec<Option<inkwell::values::PointerValue<'ctx>>>,
    /// Maps each ABI slot index to the corresponding LLVM parameter index.
    ///
    /// Gruel uses a flat native ABI where composite params (structs, arrays)
    /// occupy multiple consecutive ABI slots. In LLVM IR each Gruel param is a
    /// single aggregate value, so `Param { index: abi_slot }` must be translated
    /// to `fn_value.get_nth_param(slot_to_llvm_param[abi_slot])`.
    slot_to_llvm_param: Vec<u32>,
}

impl<'ctx, 'a> FnCodegen<'ctx, 'a> {
    fn new(
        cfg: &'a Cfg,
        fn_value: FunctionValue<'ctx>,
        ctx: &'ctx Context,
        builder: &'a Builder<'ctx>,
        module: &'a Module<'ctx>,
        type_pool: &'a TypeInternPool,
        strings: &'a [String],
        string_globals: &'a [GlobalValue<'ctx>],
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

        let num_params = cfg.num_params() as usize;
        let slot_to_llvm_param = build_slot_to_llvm_param(cfg, type_pool);
        Self {
            cfg,
            fn_value,
            ctx,
            builder,
            module,
            type_pool,
            interner,
            fn_map,
            strings,
            string_globals,
            llvm_blocks,
            values: vec![None; value_count],
            locals: vec![None; num_locals],
            phi_nodes: vec![None; value_count],
            param_allocas: vec![None; num_params],
            slot_to_llvm_param,
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

    /// Get or declare `__gruel_exit(i32) -> !`.
    fn get_or_declare_exit_fn(&self) -> FunctionValue<'ctx> {
        const NAME: &str = "__gruel_exit";
        if let Some(f) = self.module.get_function(NAME) {
            return f;
        }
        let fn_type = self.ctx.void_type().fn_type(&[self.ctx.i32_type().into()], false);
        let f = self.module.add_function(NAME, fn_type, None);
        f.add_attribute(
            inkwell::attributes::AttributeLoc::Function,
            self.ctx.create_string_attribute("noreturn", ""),
        );
        f
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

    /// Check if a Gruel type is the builtin `String` type.
    fn is_builtin_string(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Struct(id) => {
                let def = self.type_pool.struct_def(id);
                def.is_builtin && def.name == "String"
            }
            _ => false,
        }
    }

    /// Extract the `(ptr, len)` fields from a String struct value.
    ///
    /// String layout is `{ u64: ptr_as_int, u64: len, u64: cap }`.
    /// The `ptr` field is stored as `u64` (integer) in the Gruel type system.
    /// This helper extracts it as an LLVM `ptr` (via `inttoptr`) for use in
    /// runtime function calls that expect `*const u8 / *mut u8`.
    fn extract_str_ptr_len(
        &mut self,
        str_val: BasicValueEnum<'ctx>,
    ) -> (inkwell::values::PointerValue<'ctx>, inkwell::values::IntValue<'ctx>) {
        let sv = str_val.into_struct_value();
        // Field 0: ptr stored as i64 — convert to opaque ptr for runtime calls.
        let agg: AggregateValueEnum<'ctx> = sv.into();
        let ptr_as_int = self.builder
            .build_extract_value(agg, 0, "str_ptr_i")
            .expect("extract ptr field")
            .into_int_value();
        let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
        let ptr = self.builder.build_int_to_ptr(ptr_as_int, ptr_ty, "str_ptr").unwrap();
        // Field 1: len as i64.
        let agg: AggregateValueEnum<'ctx> = sv.into();
        let len = self.builder
            .build_extract_value(agg, 1, "str_len")
            .expect("extract len field")
            .into_int_value();
        (ptr, len)
    }

    /// Extract the `(ptr, len, cap)` fields from a String struct value.
    ///
    /// Same as [`extract_str_ptr_len`] but also returns the `cap` field.
    fn extract_str_ptr_len_cap(
        &mut self,
        str_val: BasicValueEnum<'ctx>,
    ) -> (inkwell::values::PointerValue<'ctx>, inkwell::values::IntValue<'ctx>, inkwell::values::IntValue<'ctx>) {
        let sv = str_val.into_struct_value();
        let agg: AggregateValueEnum<'ctx> = sv.into();
        let ptr_as_int = self.builder.build_extract_value(agg, 0, "str_ptr_i").unwrap().into_int_value();
        let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
        let ptr = self.builder.build_int_to_ptr(ptr_as_int, ptr_ty, "str_ptr").unwrap();
        let agg: AggregateValueEnum<'ctx> = sv.into();
        let len = self.builder.build_extract_value(agg, 1, "str_len").unwrap().into_int_value();
        let agg: AggregateValueEnum<'ctx> = sv.into();
        let cap = self.builder.build_extract_value(agg, 2, "str_cap").unwrap().into_int_value();
        (ptr, len, cap)
    }

    /// Get or declare `__gruel_str_eq(ptr, len, ptr, len) -> i8`.
    fn get_or_declare_str_eq(&self) -> FunctionValue<'ctx> {
        const NAME: &str = "__gruel_str_eq";
        if let Some(f) = self.module.get_function(NAME) {
            return f;
        }
        let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.ctx.i64_type();
        let fn_type = self.ctx.i8_type().fn_type(
            &[ptr_ty.into(), i64_ty.into(), ptr_ty.into(), i64_ty.into()],
            false,
        );
        self.module.add_function(NAME, fn_type, None)
    }

    /// Get or declare `__gruel_drop_String(ptr, len, cap) -> void`.
    fn get_or_declare_drop_string(&self) -> FunctionValue<'ctx> {
        const NAME: &str = "__gruel_drop_String";
        if let Some(f) = self.module.get_function(NAME) {
            return f;
        }
        let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.ctx.i64_type();
        let fn_type = self.ctx.void_type().fn_type(
            &[ptr_ty.into(), i64_ty.into(), i64_ty.into()],
            false,
        );
        self.module.add_function(NAME, fn_type, None)
    }

    /// Create an alloca in the function's entry block.
    ///
    /// Allocas in the entry block are promotable by LLVM's `mem2reg` pass and
    /// are guaranteed to dominate all uses. Used for sret slots and similar
    /// temporaries that must outlive any specific basic block.
    fn build_entry_alloca(
        &self,
        ty: inkwell::types::BasicTypeEnum<'ctx>,
        name: &str,
    ) -> inkwell::values::PointerValue<'ctx> {
        let entry_bb = self.llvm_blocks[0];
        let current_bb = self.builder.get_insert_block();
        match entry_bb.get_first_instruction() {
            Some(first) => self.builder.position_before(&first),
            None => self.builder.position_at_end(entry_bb),
        }
        let ptr = self.builder.build_alloca(ty, name).expect("build_alloca failed");
        if let Some(bb) = current_bb {
            self.builder.position_at_end(bb);
        }
        ptr
    }

    /// Emit a runtime array bounds check.
    ///
    /// If `index >= length`, calls `__gruel_bounds_check()` (which never returns).
    /// Uses unsigned comparison so negative indices (interpreted as large positives) also fail.
    fn build_bounds_check(&mut self, index: inkwell::values::IntValue<'ctx>, length: u64) {
        let i64_ty = self.ctx.i64_type();
        // Extend or truncate the index to i64 for comparison.
        let bits = index.get_type().get_bit_width();
        let idx_i64 = if bits < 64 {
            self.builder.build_int_z_extend(index, i64_ty, "bidx").unwrap()
        } else if bits > 64 {
            self.builder.build_int_truncate(index, i64_ty, "bidx").unwrap()
        } else {
            index
        };
        let len_val = i64_ty.const_int(length, false);
        let in_bounds = self.builder
            .build_int_compare(IntPredicate::ULT, idx_i64, len_val, "bchk")
            .unwrap();

        let current_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let ok_bb = self.ctx.append_basic_block(current_fn, "binbounds");
        let oob_bb = self.ctx.append_basic_block(current_fn, "boob");
        self.builder.build_conditional_branch(in_bounds, ok_bb, oob_bb).unwrap();

        // Out-of-bounds handler: call __gruel_bounds_check() then unreachable.
        self.builder.position_at_end(oob_bb);
        let check_fn = self.get_or_declare_noreturn_fn("__gruel_bounds_check");
        self.builder.build_call(check_fn, &[], "").unwrap();
        self.builder.build_unreachable().unwrap();

        // Continue in the in-bounds block.
        self.builder.position_at_end(ok_bb);
    }

    /// Zero-extend or truncate `index` to `i64` for use in GEP instructions.
    fn index_to_i64(&self, index: inkwell::values::IntValue<'ctx>) -> inkwell::values::IntValue<'ctx> {
        let i64_ty = self.ctx.i64_type();
        let bits = index.get_type().get_bit_width();
        if bits < 64 {
            self.builder.build_int_z_extend(index, i64_ty, "iidx").unwrap()
        } else if bits > 64 {
            self.builder.build_int_truncate(index, i64_ty, "iidx").unwrap()
        } else {
            index
        }
    }

    /// Compute the LLVM field index from a Gruel (declaration-order) field index.
    ///
    /// The LLVM struct type only contains non-void fields, so the LLVM field
    /// index may be less than the Gruel field index if any preceding fields
    /// are void-typed (unit type, etc.).
    fn gruel_to_llvm_field_index(&self, struct_id: StructId, gruel_field_index: u32) -> u32 {
        let struct_def = self.type_pool.struct_def(struct_id);
        struct_def.fields[..gruel_field_index as usize]
            .iter()
            .filter(|f| gruel_type_to_llvm(f.ty, self.ctx, self.type_pool).is_some())
            .count() as u32
    }

    /// Get or create an alloca for a value parameter, for GEP field/index access.
    ///
    /// Non-inout params are passed by value in LLVM. When we need to GEP into
    /// them (field access or array indexing), we spill the param value to an
    /// alloca and return a pointer to it.
    fn get_or_create_param_alloca(
        &mut self,
        param_slot: u32,
        param_ty: Type,
    ) -> inkwell::values::PointerValue<'ctx> {
        let slot = param_slot as usize;
        if let Some(ptr) = self.param_allocas[slot] {
            return ptr;
        }

        // Insert alloca in the entry block (before any instruction).
        let entry_bb = self.llvm_blocks[0];
        let current_bb = self.builder.get_insert_block();
        match entry_bb.get_first_instruction() {
            Some(first) => self.builder.position_before(&first),
            None => self.builder.position_at_end(entry_bb),
        }
        let llvm_ty = gruel_type_to_llvm(param_ty, self.ctx, self.type_pool)
            .expect("param alloca type must be non-void");
        let ptr = self.builder
            .build_alloca(llvm_ty, &format!("pslot{}", slot))
            .expect("build_alloca failed");
        if let Some(bb) = current_bb {
            self.builder.position_at_end(bb);
        }

        // Spill the fn param value into the alloca so GEP can address into it.
        let llvm_param_idx = self.slot_to_llvm_param[slot];
        let param_val = self.fn_value
            .get_nth_param(llvm_param_idx)
            .expect("param slot out of range");
        self.builder.build_store(ptr, param_val).unwrap();

        self.param_allocas[slot] = Some(ptr);
        ptr
    }

    /// Walk a Place's projections and produce the final GEP pointer.
    ///
    /// Returns `None` if the base type has no LLVM representation (void-typed place).
    fn build_place_gep_chain(
        &mut self,
        place: &gruel_cfg::Place,
        result_ty: Type,
    ) -> Option<inkwell::values::PointerValue<'ctx>> {
        let projections = self.cfg.get_place_projections(place).to_vec();

        if projections.is_empty() {
            // No projections: base pointer is the destination.
            return match place.base {
                PlaceBase::Local(slot) => self.locals.get(slot as usize).copied().flatten(),
                PlaceBase::Param(param_slot) => {
                    if self.cfg.is_param_inout(param_slot) {
                        let llvm_idx = self.slot_to_llvm_param[param_slot as usize];
                        Some(self.fn_value.get_nth_param(llvm_idx)?.into_pointer_value())
                    } else {
                        Some(self.get_or_create_param_alloca(param_slot, result_ty))
                    }
                }
            };
        }

        // Determine the container type from the first projection.
        let base_container_ty = match &projections[0] {
            Projection::Field { struct_id, .. } => Type::new_struct(*struct_id),
            Projection::Index { array_type, .. } => *array_type,
        };

        // Get the base pointer.
        let mut current_ptr: inkwell::values::PointerValue<'ctx> = match place.base {
            PlaceBase::Local(slot) => self.get_or_create_local(slot, base_container_ty),
            PlaceBase::Param(param_slot) => {
                if self.cfg.is_param_inout(param_slot) {
                    let llvm_idx = self.slot_to_llvm_param[param_slot as usize];
                    self.fn_value.get_nth_param(llvm_idx)?.into_pointer_value()
                } else {
                    self.get_or_create_param_alloca(param_slot, base_container_ty)
                }
            }
        };

        // Walk projections, building GEP instructions.
        let mut current_ty = base_container_ty;
        for proj in &projections {
            match proj {
                Projection::Field { struct_id, field_index } => {
                    let llvm_idx = self.gruel_to_llvm_field_index(*struct_id, *field_index);
                    let struct_llvm_ty = gruel_type_to_llvm(current_ty, self.ctx, self.type_pool)?
                        .into_struct_type();
                    current_ptr = self.builder
                        .build_struct_gep(struct_llvm_ty, current_ptr, llvm_idx, "fgep")
                        .expect("build_struct_gep failed");
                    let struct_def = self.type_pool.struct_def(*struct_id);
                    current_ty = struct_def.fields[*field_index as usize].ty;
                }
                Projection::Index { array_type, index } => {
                    // Bounds check.
                    let arr_id = array_type.as_array().expect("Index on non-array");
                    let (elem_ty, length) = self.type_pool.array_def(arr_id);
                    let index_val = self.get_value(*index).into_int_value();
                    self.build_bounds_check(index_val, length);
                    // GEP into the array: [0, index].
                    let arr_llvm_ty = gruel_type_to_llvm(*array_type, self.ctx, self.type_pool)?
                        .into_array_type();
                    let zero = self.ctx.i64_type().const_zero();
                    let idx_i64 = self.index_to_i64(index_val);
                    current_ptr = unsafe {
                        self.builder
                            .build_gep(arr_llvm_ty, current_ptr, &[zero, idx_i64], "igep")
                            .expect("build_gep failed")
                    };
                    current_ty = elem_ty;
                }
            }
        }

        Some(current_ptr)
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

    /// Recursively compare two values of the same Gruel type for equality.
    ///
    /// Returns an `i1` value (`true` when equal).
    ///
    /// - Scalars (int, bool, enum): `icmp eq`
    /// - Structs: field-by-field `icmp eq` ANDed together
    /// - Arrays: element-by-element `icmp eq` ANDed together
    fn build_value_eq(
        &mut self,
        ty: Type,
        l: BasicValueEnum<'ctx>,
        r: BasicValueEnum<'ctx>,
    ) -> inkwell::values::IntValue<'ctx> {
        match ty.kind() {
            TypeKind::Struct(id) => {
                let struct_def = self.type_pool.struct_def(id).clone();
                // String equality uses __gruel_str_eq (content comparison, not field comparison).
                if struct_def.is_builtin && struct_def.name == "String" {
                    let (ptr1, len1) = self.extract_str_ptr_len(l);
                    let (ptr2, len2) = self.extract_str_ptr_len(r);
                    let str_eq_fn = self.get_or_declare_str_eq();
                    let result = self.builder
                        .build_call(str_eq_fn, &[ptr1.into(), len1.into(), ptr2.into(), len2.into()], "streq")
                        .unwrap();
                    let byte_val = result.try_as_basic_value().basic().unwrap().into_int_value();
                    // __gruel_str_eq returns i8; convert to i1 for use as a bool.
                    let zero = self.ctx.i8_type().const_zero();
                    return self.builder.build_int_compare(IntPredicate::NE, byte_val, zero, "streq_b").unwrap();
                }
                let mut all_eq = self.ctx.bool_type().const_int(1, false); // start true
                let mut llvm_idx = 0u32;
                for field in &struct_def.fields {
                    if gruel_type_to_llvm(field.ty, self.ctx, self.type_pool).is_none() {
                        continue; // skip void fields
                    }
                    let l_agg: AggregateValueEnum<'ctx> = l.into_struct_value().into();
                    let r_agg: AggregateValueEnum<'ctx> = r.into_struct_value().into();
                    let l_field = self.builder
                        .build_extract_value(l_agg, llvm_idx, "l_f")
                        .expect("build_extract_value failed");
                    let r_field = self.builder
                        .build_extract_value(r_agg, llvm_idx, "r_f")
                        .expect("build_extract_value failed");
                    let field_eq = self.build_value_eq(field.ty, l_field, r_field);
                    all_eq = self.builder.build_and(all_eq, field_eq, "and_eq").unwrap();
                    llvm_idx += 1;
                }
                all_eq
            }
            TypeKind::Array(id) => {
                let (elem_ty, len) = self.type_pool.array_def(id);
                let mut all_eq = self.ctx.bool_type().const_int(1, false);
                for i in 0..len as u32 {
                    let l_agg: AggregateValueEnum<'ctx> = l.into_array_value().into();
                    let r_agg: AggregateValueEnum<'ctx> = r.into_array_value().into();
                    let l_elem = self.builder
                        .build_extract_value(l_agg, i, "l_e")
                        .expect("build_extract_value failed");
                    let r_elem = self.builder
                        .build_extract_value(r_agg, i, "r_e")
                        .expect("build_extract_value failed");
                    let elem_eq = self.build_value_eq(elem_ty, l_elem, r_elem);
                    all_eq = self.builder.build_and(all_eq, elem_eq, "and_eq").unwrap();
                }
                all_eq
            }
            _ => {
                // Scalar: icmp eq on integers (bool, enums, ints all map to LLVM int types).
                let l_int = l.into_int_value();
                let r_int = r.into_int_value();
                self.builder
                    .build_int_compare(IntPredicate::EQ, l_int, r_int, "eq")
                    .unwrap()
            }
        }
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

            CfgInstData::StringConst(idx) => {
                // Build a String struct value `{ ptr, len, 0 }` pointing at the
                // LLVM global that holds the string's raw bytes.
                let idx = idx as usize;
                let str_len = self.strings.get(idx).map(|s| s.len()).unwrap_or(0) as u64;
                let global = self.string_globals.get(idx);
                let i64_ty = self.ctx.i64_type();
                // String.ptr is stored as u64 (integer), not as ptr type.
                // Convert the global address to i64 via ptrtoint so it fits in
                // the String struct's first field.
                let data_ptr_as_int: inkwell::values::BasicValueEnum<'ctx> = if let Some(g) = global {
                    let raw_ptr = g.as_pointer_value();
                    self.builder.build_ptr_to_int(raw_ptr, i64_ty, "str_data_i").unwrap().into()
                } else {
                    i64_ty.const_zero().into()
                };
                let len_val: inkwell::values::BasicValueEnum<'ctx> = i64_ty.const_int(str_len, false).into();
                let cap_val: inkwell::values::BasicValueEnum<'ctx> = i64_ty.const_zero().into();
                // Build String struct { ptr, i64, i64 }
                let str_llvm_ty = gruel_type_to_llvm(ty, self.ctx, self.type_pool)
                    .expect("String type must have LLVM representation")
                    .into_struct_type();
                let undef: AggregateValueEnum<'ctx> = str_llvm_ty.get_undef().into();
                let agg = self.builder.build_insert_value(undef, data_ptr_as_int, 0, "sc_ptr").unwrap();
                let agg = self.builder.build_insert_value(agg, len_val, 1, "sc_len").unwrap();
                let agg = self.builder.build_insert_value(agg, cap_val, 2, "sc_cap").unwrap();
                Some(agg.as_basic_value_enum())
            }

            CfgInstData::Param { index } => {
                let llvm_idx = self.slot_to_llvm_param[index as usize];
                let param_val = self.fn_value.get_nth_param(llvm_idx)
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
                    let l = self.get_value(lhs);
                    let r = self.get_value(rhs);
                    Some(self.build_value_eq(lhs_ty, l, r).into())
                }
            }
            CfgInstData::Ne(lhs, rhs) => {
                let lhs_ty = self.cfg.get_inst(lhs).ty;
                if gruel_type_to_llvm(lhs_ty, self.ctx, self.type_pool).is_none() {
                    // Unit != Unit is always false.
                    Some(self.ctx.bool_type().const_int(0, false).into())
                } else {
                    let l = self.get_value(lhs);
                    let r = self.get_value(rhs);
                    let eq = self.build_value_eq(lhs_ty, l, r);
                    // Ne = not(Eq)
                    Some(self.builder.build_not(eq, "ne").unwrap().into())
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
                let llvm_idx = self.slot_to_llvm_param[param_slot as usize];
                let ptr_val = self.fn_value.get_nth_param(llvm_idx)
                    .expect("param_slot out of range")
                    .into_pointer_value();
                self.builder.build_store(ptr_val, v).unwrap();
                None
            }

            // ---- Function calls ----
            CfgInstData::Call { name, args_start, args_len } => {
                let fn_name = self.interner.resolve(&name);

                let args = self.cfg.get_call_args(args_start, args_len).to_vec();
                let call_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = args
                    .iter()
                    .filter_map(|arg| {
                        if arg.is_inout() || arg.is_borrow() {
                            // By-ref: pass the raw pointer, not the loaded value.
                            let inst = self.cfg.get_inst(arg.value);
                            return match inst.data {
                                CfgInstData::Load { slot } => {
                                    // Arg is a local variable: pass its alloca.
                                    let ptr = self.locals[slot as usize]
                                        .expect("inout/borrow arg: slot not yet allocated");
                                    Some(inkwell::values::BasicMetadataValueEnum::from(ptr))
                                }
                                CfgInstData::Param { index } if self.cfg.is_param_inout(index) => {
                                    // Forwarding an inout/borrow param: pass the raw pointer.
                                    let llvm_idx = self.slot_to_llvm_param[index as usize];
                                    let ptr = self.fn_value
                                        .get_nth_param(llvm_idx)
                                        .expect("param slot out of range");
                                    Some(inkwell::values::BasicMetadataValueEnum::from(ptr))
                                }
                                _ => {
                                    // Fallback: pass the value as-is.
                                    Some(self.get_value(arg.value).into())
                                }
                            };
                        }
                        // Skip unit-typed (void) args — they have no LLVM representation.
                        let arg_ty = self.cfg.get_inst(arg.value).ty;
                        if gruel_type_to_llvm(arg_ty, self.ctx, self.type_pool).is_none() {
                            return None;
                        }
                        Some(self.get_value(arg.value).into())
                    })
                    .collect();

                // Detect whether this is an external C function that returns a struct.
                // External C functions that return structs use the sret convention:
                // the caller allocates space and passes a pointer as the first argument,
                // and the callee writes the struct there (void return in LLVM IR).
                // Gruel's own functions (in fn_map) handle their own ABI; only external
                // functions need sret treatment.
                let is_gruel_fn = self.fn_map.contains_key(fn_name);
                let ret_llvm = gruel_type_to_llvm(ty, self.ctx, self.type_pool);
                let ret_is_struct = matches!(ret_llvm, Some(inkwell::types::BasicTypeEnum::StructType(_)));

                if !is_gruel_fn && ret_is_struct {
                    // sret pattern: allocate space in entry block, pass as hidden first arg.
                    let struct_ty = ret_llvm.unwrap().into_struct_type();
                    let sret_ptr = self.build_entry_alloca(struct_ty.into(), "sret");

                    // Build param types: ptr (sret) + the regular args.
                    let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
                    let mut sret_param_types: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> =
                        vec![ptr_ty.into()];
                    sret_param_types.extend(args.iter().filter_map(|arg| {
                        if arg.is_inout() || arg.is_borrow() {
                            Some(ptr_ty.into())
                        } else {
                            let arg_ty = self.cfg.get_inst(arg.value).ty;
                            gruel_type_to_llvm_param(arg_ty, self.ctx, self.type_pool)
                        }
                    }));

                    // Look up or declare as void fn(ptr, ...).
                    let callee = self.module.get_function(fn_name).unwrap_or_else(|| {
                        let fn_ty = self.ctx.void_type().fn_type(&sret_param_types, false);
                        self.module.add_function(fn_name, fn_ty, None)
                    });

                    // Build call args: sret pointer first, then value args.
                    let mut sret_call_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> =
                        vec![sret_ptr.into()];
                    sret_call_args.extend(call_args.iter().copied());

                    self.builder.build_call(callee, &sret_call_args, "").unwrap();

                    // Load the result struct from the sret alloca.
                    let loaded = self.builder
                        .build_load(struct_ty, sret_ptr, "sret_load")
                        .unwrap();
                    Some(loaded)
                } else {
                    // Normal call: look up in the declared-functions map, then fall back to the
                    // module. If not found anywhere, auto-declare as an external function.
                    let callee = self.fn_map.get(fn_name).copied()
                        .or_else(|| self.module.get_function(fn_name))
                        .unwrap_or_else(|| {
                            // Infer LLVM param types from the Gruel arg types.
                            let param_types: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> = args
                                .iter()
                                .filter_map(|arg| {
                                    if arg.is_inout() || arg.is_borrow() {
                                        Some(self.ctx.ptr_type(inkwell::AddressSpace::default()).into())
                                    } else {
                                        let arg_ty = self.cfg.get_inst(arg.value).ty;
                                        gruel_type_to_llvm_param(arg_ty, self.ctx, self.type_pool)
                                    }
                                })
                                .collect();
                            let fn_ty = match ret_llvm {
                                Some(ret) => ret.fn_type(&param_types, false),
                                None => self.ctx.void_type().fn_type(&param_types, false),
                            };
                            self.module.add_function(fn_name, fn_ty, None)
                        });

                    let call_site = self.builder.build_call(callee, &call_args, "call").unwrap();
                    // `basic()` returns Some for non-void calls, None for void.
                    call_site.try_as_basic_value().basic()
                }
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
                    // Same-width cast between signed and unsigned types.
                    // Check that the value is representable in the destination type.
                    let fits = if is_signed_type(from_ty) && !is_signed_type(ty) {
                        // Signed → Unsigned: overflow if value < 0.
                        let zero = v.get_type().const_zero();
                        self.builder
                            .build_int_compare(IntPredicate::SGE, v, zero, "ick_fits")
                            .unwrap()
                    } else if !is_signed_type(from_ty) && is_signed_type(ty) {
                        // Unsigned → Signed: overflow if value > INT_MAX.
                        let int_max_val = (i64::MAX as u64) >> (64u32.saturating_sub(src_bits));
                        let max = v.get_type().const_int(int_max_val, false);
                        self.builder
                            .build_int_compare(IntPredicate::ULE, v, max, "ick_fits")
                            .unwrap()
                    } else {
                        // Same sign (shouldn't happen in valid Gruel) — no overflow.
                        v.get_type().const_int(1, false)
                    };
                    // Branch to overflow handler if the value is out of range.
                    let current_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                    let overflow_bb = self.ctx.append_basic_block(current_fn, "icast_ovf");
                    let cont_bb = self.ctx.append_basic_block(current_fn, "icast_cont");
                    self.builder.build_conditional_branch(fits, cont_bb, overflow_bb).unwrap();
                    self.builder.position_at_end(overflow_bb);
                    let panic_fn = self.get_or_declare_noreturn_fn("__gruel_intcast_overflow");
                    self.builder.build_call(panic_fn, &[], "").unwrap();
                    self.builder.build_unreachable().unwrap();
                    self.builder.position_at_end(cont_bb);
                    v // Return original bits (reinterpreted as destination type)
                };
                Some(result.into())
            }

            // ---- Drop / storage liveness ----
            CfgInstData::Drop { value: dropped_value } => {
                let dropped_ty = self.cfg.get_inst(dropped_value).ty;
                if self.is_builtin_string(dropped_ty) {
                    // Only drop heap-allocated strings (cap > 0).
                    // Literals have cap == 0, so __gruel_drop_String is a no-op for them,
                    // but it's safe to call unconditionally.
                    if let Some(str_val) = self.values[dropped_value.as_u32() as usize] {
                        let (ptr, len, cap) = self.extract_str_ptr_len_cap(str_val);
                        let drop_fn = self.get_or_declare_drop_string();
                        self.builder.build_call(drop_fn, &[ptr.into(), len.into(), cap.into()], "").unwrap();
                    }
                }
                None
            }
            CfgInstData::StorageLive { .. }
            | CfgInstData::StorageDead { .. } => None,

            // ---- Composite ops (Phase 2d) ----

            CfgInstData::EnumVariant { variant_index, .. } => {
                // Enums are stored as their discriminant integer.
                // The LLVM type comes from the enum's discriminant_type() via gruel_type_to_llvm.
                gruel_type_to_llvm(ty, self.ctx, self.type_pool)
                    .map(|t| t.into_int_type().const_int(variant_index as u64, false).into())
            }

            CfgInstData::StructInit { struct_id, fields_start, fields_len } => {
                let fields = self.cfg.get_extra(fields_start, fields_len).to_vec();
                let struct_def = self.type_pool.struct_def(struct_id);
                let struct_llvm_ty = match gruel_type_to_llvm(ty, self.ctx, self.type_pool) {
                    Some(t) => t.into_struct_type(),
                    None => return Ok(()), // void struct — no representation
                };
                let mut agg: AggregateValueEnum = struct_llvm_ty.get_undef().into();
                let mut llvm_idx = 0u32;
                for (gruel_idx, &field_val) in fields.iter().enumerate() {
                    let field_ty = struct_def.fields[gruel_idx].ty;
                    if gruel_type_to_llvm(field_ty, self.ctx, self.type_pool).is_some() {
                        let fv = self.get_value(field_val);
                        agg = self.builder
                            .build_insert_value(agg, fv, llvm_idx, "si")
                            .expect("build_insert_value failed");
                        llvm_idx += 1;
                    }
                }
                Some(agg.as_basic_value_enum())
            }

            CfgInstData::ArrayInit { elements_start, elements_len } => {
                let elements = self.cfg.get_extra(elements_start, elements_len).to_vec();
                let arr_llvm_ty = match gruel_type_to_llvm(ty, self.ctx, self.type_pool) {
                    Some(t) => t.into_array_type(),
                    None => return Ok(()), // void array — no representation
                };
                let mut agg: AggregateValueEnum = arr_llvm_ty.get_undef().into();
                for (i, &elem_val) in elements.iter().enumerate() {
                    let v = self.get_value(elem_val);
                    agg = self.builder
                        .build_insert_value(agg, v, i as u32, "ai")
                        .expect("build_insert_value failed");
                }
                Some(agg.as_basic_value_enum())
            }

            CfgInstData::PlaceRead { place } => {
                let ptr = match self.build_place_gep_chain(&place, ty) {
                    Some(p) => p,
                    None => return Ok(()), // void-typed place
                };
                match gruel_type_to_llvm(ty, self.ctx, self.type_pool) {
                    None => None,
                    Some(llvm_ty) => Some(self.builder.build_load(llvm_ty, ptr, "prld").unwrap()),
                }
            }

            CfgInstData::PlaceWrite { place, value: val } => {
                let val_ty = self.cfg.get_inst(val).ty;
                if gruel_type_to_llvm(val_ty, self.ctx, self.type_pool).is_some() {
                    let v = self.get_value(val);
                    if let Some(ptr) = self.build_place_gep_chain(&place, val_ty) {
                        self.builder.build_store(ptr, v).unwrap();
                    }
                }
                None
            }

            CfgInstData::FieldSet { slot, struct_id, field_index, value: val } => {
                let val_ty = self.cfg.get_inst(val).ty;
                if gruel_type_to_llvm(val_ty, self.ctx, self.type_pool).is_some() {
                    let v = self.get_value(val);
                    let struct_ty = Type::new_struct(struct_id);
                    let ptr = self.get_or_create_local(slot, struct_ty);
                    let llvm_field_idx = self.gruel_to_llvm_field_index(struct_id, field_index);
                    let struct_llvm_ty = gruel_type_to_llvm(struct_ty, self.ctx, self.type_pool)
                        .expect("struct must have LLVM type")
                        .into_struct_type();
                    let field_ptr = self.builder
                        .build_struct_gep(struct_llvm_ty, ptr, llvm_field_idx, "fsgep")
                        .expect("build_struct_gep failed");
                    self.builder.build_store(field_ptr, v).unwrap();
                }
                None
            }

            CfgInstData::IndexSet { slot, array_type, index, value: val } => {
                let val_ty = self.cfg.get_inst(val).ty;
                if gruel_type_to_llvm(val_ty, self.ctx, self.type_pool).is_some() {
                    let v = self.get_value(val);
                    let arr_id = array_type.as_array().expect("IndexSet on non-array");
                    let (_elem_ty, length) = self.type_pool.array_def(arr_id);
                    let index_val = self.get_value(index).into_int_value();
                    self.build_bounds_check(index_val, length);
                    let ptr = self.get_or_create_local(slot, array_type);
                    let arr_llvm_ty = gruel_type_to_llvm(array_type, self.ctx, self.type_pool)
                        .expect("array must have LLVM type")
                        .into_array_type();
                    let zero = self.ctx.i64_type().const_zero();
                    let idx_i64 = self.index_to_i64(index_val);
                    let elem_ptr = unsafe {
                        self.builder
                            .build_gep(arr_llvm_ty, ptr, &[zero, idx_i64], "isgep")
                            .expect("build_gep failed")
                    };
                    self.builder.build_store(elem_ptr, v).unwrap();
                }
                None
            }

            // ParamFieldSet and ParamIndexSet are legacy instructions that are
            // never generated by sema. Handle them defensively just in case.
            CfgInstData::ParamFieldSet { param_slot, struct_id, field_index, value: val, .. } => {
                let val_ty = self.cfg.get_inst(val).ty;
                if gruel_type_to_llvm(val_ty, self.ctx, self.type_pool).is_some() {
                    let v = self.get_value(val);
                    let struct_ty = Type::new_struct(struct_id);
                    let llvm_idx = self.slot_to_llvm_param[param_slot as usize];
                    let base_ptr = if self.cfg.is_param_inout(param_slot) {
                        self.fn_value.get_nth_param(llvm_idx)
                            .expect("param slot out of range")
                            .into_pointer_value()
                    } else {
                        self.get_or_create_param_alloca(param_slot, struct_ty)
                    };
                    let llvm_field_idx = self.gruel_to_llvm_field_index(struct_id, field_index);
                    let struct_llvm_ty = gruel_type_to_llvm(struct_ty, self.ctx, self.type_pool)
                        .expect("struct must have LLVM type")
                        .into_struct_type();
                    let field_ptr = self.builder
                        .build_struct_gep(struct_llvm_ty, base_ptr, llvm_field_idx, "pfsgep")
                        .expect("build_struct_gep failed");
                    self.builder.build_store(field_ptr, v).unwrap();
                }
                None
            }

            CfgInstData::ParamIndexSet { param_slot, array_type, index, value: val } => {
                let val_ty = self.cfg.get_inst(val).ty;
                if gruel_type_to_llvm(val_ty, self.ctx, self.type_pool).is_some() {
                    let v = self.get_value(val);
                    let arr_id = array_type.as_array().expect("ParamIndexSet on non-array");
                    let (_elem_ty, length) = self.type_pool.array_def(arr_id);
                    let index_val = self.get_value(index).into_int_value();
                    self.build_bounds_check(index_val, length);
                    let llvm_idx2 = self.slot_to_llvm_param[param_slot as usize];
                    let base_ptr = if self.cfg.is_param_inout(param_slot) {
                        self.fn_value.get_nth_param(llvm_idx2)
                            .expect("param slot out of range")
                            .into_pointer_value()
                    } else {
                        self.get_or_create_param_alloca(param_slot, array_type)
                    };
                    let arr_llvm_ty = gruel_type_to_llvm(array_type, self.ctx, self.type_pool)
                        .expect("array must have LLVM type")
                        .into_array_type();
                    let zero = self.ctx.i64_type().const_zero();
                    let idx_i64 = self.index_to_i64(index_val);
                    let elem_ptr = unsafe {
                        self.builder
                            .build_gep(arr_llvm_ty, base_ptr, &[zero, idx_i64], "pisgep")
                            .expect("build_gep failed")
                    };
                    self.builder.build_store(elem_ptr, v).unwrap();
                }
                None
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
                        _ if self.is_builtin_string(arg_ty) => {
                            // String: call __gruel_dbg_str(ptr, len)
                            let str_val = self.get_value(arg_val);
                            let (ptr, len) = self.extract_str_ptr_len(str_val);
                            let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
                            let fn_ty = self.ctx.void_type().fn_type(&[ptr_ty.into(), i64_ty.into()], false);
                            let f = self.module.get_function("__gruel_dbg_str")
                                .unwrap_or_else(|| self.module.add_function("__gruel_dbg_str", fn_ty, None));
                            self.builder.build_call(f, &[ptr.into(), len.into()], "").unwrap();
                        }
                        _ => {
                            // Arrays and non-String structs are not supported by @dbg.
                            // This matches the native backend's ICE for unsupported types.
                            unreachable!("@dbg: unsupported type {:?}", arg_ty.kind());
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

            // ---- Address-of (raw pointer to any lvalue) ----
            "raw" | "raw_mut" => {
                let lvalue_val = args[0];
                let lvalue_inst = self.cfg.get_inst(lvalue_val).clone();
                match lvalue_inst.data {
                    CfgInstData::Load { slot } => {
                        // Plain local variable: return its alloca pointer.
                        let slot_ty = lvalue_inst.ty;
                        let ptr = self.get_or_create_local(slot, slot_ty);
                        Some(ptr.into())
                    }
                    CfgInstData::PlaceRead { ref place } => {
                        // Composite lvalue (struct field or array element):
                        // return the GEP pointer into the storage, not the value.
                        let place = place.clone();
                        let elem_ty = lvalue_inst.ty;
                        self.build_place_gep_chain(&place, elem_ty).map(Into::into)
                    }
                    _ => {
                        // Fallback: return a null pointer.
                        gruel_type_to_llvm(ty, self.ctx, self.type_pool).map(|t| t.const_zero())
                    }
                }
            }

            // ---- String parsing intrinsics ----
            "parse_i32" | "parse_i64" | "parse_u32" | "parse_u64" => {
                // @parse_*(s: String) -> integer
                // Extract ptr and len from the String struct, then call __gruel_parse_*
                let str_val = self.get_value(args[0]);
                let (ptr, len) = self.extract_str_ptr_len(str_val);
                let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.ctx.i64_type();
                let (runtime_fn, ret_llvm_ty): (&str, inkwell::types::BasicMetadataTypeEnum<'ctx>) = match name_str {
                    "parse_i32" => ("__gruel_parse_i32", self.ctx.i32_type().into()),
                    "parse_i64" => ("__gruel_parse_i64", i64_ty.into()),
                    "parse_u32" => ("__gruel_parse_u32", self.ctx.i32_type().into()),
                    "parse_u64" => ("__gruel_parse_u64", i64_ty.into()),
                    _ => unreachable!(),
                };
                let fn_ty_ret = match name_str {
                    "parse_i32" | "parse_u32" => self.ctx.i32_type().fn_type(&[ptr_ty.into(), i64_ty.into()], false),
                    _ => i64_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false),
                };
                let _ = ret_llvm_ty; // suppress unused warning
                let f = self.module.get_function(runtime_fn)
                    .unwrap_or_else(|| self.module.add_function(runtime_fn, fn_ty_ret, None));
                let result = self.builder.build_call(f, &[ptr.into(), len.into()], "parsed").unwrap();
                result.try_as_basic_value().basic()
            }

            // ---- Read line from stdin ----
            "read_line" => {
                // @read_line() -> String
                // Allocate space for the String struct on the stack, call __gruel_read_line(out_ptr),
                // then load the resulting struct.
                let str_llvm_ty = gruel_type_to_llvm(ty, self.ctx, self.type_pool)
                    .expect("read_line must return String (non-void)")
                    .into_struct_type();

                // Alloca in the entry block.
                let entry_bb = self.llvm_blocks[0];
                let current_bb = self.builder.get_insert_block();
                match entry_bb.get_first_instruction() {
                    Some(first) => self.builder.position_before(&first),
                    None => self.builder.position_at_end(entry_bb),
                }
                let sret_ptr = self.builder.build_alloca(str_llvm_ty, "rl_sret").unwrap();
                if let Some(bb) = current_bb {
                    self.builder.position_at_end(bb);
                }

                let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
                let fn_ty = self.ctx.void_type().fn_type(&[ptr_ty.into()], false);
                let f = self.module.get_function("__gruel_read_line")
                    .unwrap_or_else(|| self.module.add_function("__gruel_read_line", fn_ty, None));
                self.builder.build_call(f, &[sret_ptr.into()], "").unwrap();

                // Load the String struct from the sret alloca.
                Some(self.builder.build_load(str_llvm_ty, sret_ptr, "rl_str").unwrap())
            }

            // ---- Fallback: return zero value for unimplemented intrinsics ----
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
                    // Unit-typed return value.
                    // For `fn main() { }` the runtime reads eax/w0 after the call,
                    // so a bare `ret void` leaves the exit code undefined.
                    // Call __gruel_exit(0) so the process exits cleanly.
                    if self.cfg.fn_name() == "main" {
                        let exit_fn = self.get_or_declare_exit_fn();
                        let zero = self.ctx.i32_type().const_zero();
                        self.builder.build_call(exit_fn, &[zero.into()], "").unwrap();
                        self.builder.build_unreachable().unwrap();
                    } else {
                        self.builder.build_return(None).unwrap();
                    }
                } else {
                    let ret_val = self.get_value(v);
                    self.builder.build_return(Some(&ret_val)).unwrap();
                }
            }
            Terminator::Return { value: None } => {
                // Unit return (no explicit return value).
                if self.cfg.fn_name() == "main" {
                    let exit_fn = self.get_or_declare_exit_fn();
                    let zero = self.ctx.i32_type().const_zero();
                    self.builder.build_call(exit_fn, &[zero.into()], "").unwrap();
                    self.builder.build_unreachable().unwrap();
                } else {
                    self.builder.build_return(None).unwrap();
                }
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
    string_globals: &[GlobalValue<'ctx>],
    interner: &ThreadedRodeo,
    fn_map: &HashMap<&str, FunctionValue<'ctx>>,
) -> CompileResult<()> {
    let mut fn_gen = FnCodegen::new(cfg, *fn_value, ctx, builder, module, type_pool, strings, string_globals, interner, fn_map);
    fn_gen.translate()
}

/// Returns true for signed integer types.
fn is_signed_type(ty: Type) -> bool {
    matches!(
        ty.kind(),
        TypeKind::I8 | TypeKind::I16 | TypeKind::I32 | TypeKind::I64
    )
}

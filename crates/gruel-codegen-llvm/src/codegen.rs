//! LLVM IR generation from Gruel CFG.
//!
//! This module generates LLVM IR from the Gruel CFG.

use rustc_hash::FxHashMap as HashMap;

use gruel_air::layout::{DiscriminantStrategy, layout_of};
use gruel_air::{StructId, Type, TypeInternPool, TypeKind};
use gruel_cfg::{
    BlockId, Cfg, CfgInstData, CfgValue, OptLevel, PlaceBase, Projection, Terminator, drop_names,
};
use gruel_intrinsics::{IntrinsicId, lookup_by_name};
use gruel_util::{BinOp, CompileError, CompileResult, ErrorKind, UnaryOp};
use inkwell::FloatPredicate;
use inkwell::IntPredicate;
use inkwell::OptimizationLevel;
use inkwell::basic_block::BasicBlock as LlvmBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::intrinsics::Intrinsic;
use inkwell::module::Module;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target as LlvmTarget, TargetMachine,
};
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::BasicMetadataValueEnum;
use inkwell::values::{
    AggregateValueEnum, BasicValue, BasicValueEnum, FunctionValue, GlobalValue, PhiValue,
};
use lasso::ThreadedRodeo;

use crate::CodegenInputs;
use crate::types::{gruel_type_to_llvm, gruel_type_to_llvm_param};

/// Convert an LLVM-related error string into a [`CompileError`].
fn llvm_error(msg: impl Into<String>) -> CompileError {
    CompileError::without_span(ErrorKind::InternalError(msg.into()))
}

/// Module-level state shared across every function being codegen'd.
///
/// Built once in `build_module` after globals are emitted, then handed to
/// `define_function` for each function. Bundled to avoid 14-arg signatures.
struct ModuleCtx<'ctx, 'a> {
    ctx: &'ctx Context,
    builder: &'a Builder<'ctx>,
    module: &'a Module<'ctx>,
    type_pool: &'a TypeInternPool,
    interner: &'a ThreadedRodeo,
    strings: &'a [String],
    string_globals: &'a [GlobalValue<'ctx>],
    bytes_globals: &'a [GlobalValue<'ctx>],
    bytes_lens: &'a [u64],
    fn_map: &'a HashMap<&'a str, FunctionValue<'ctx>>,
    vtable_map: &'a HashMap<(gruel_air::StructId, gruel_air::InterfaceId), GlobalValue<'ctx>>,
    interface_defs: &'a [gruel_air::InterfaceDef],
}

/// Build an LLVM module from a set of function CFGs.
///
/// This is the shared core for both [`generate`] (object emission) and
/// [`generate_ir`] (textual IR emission). Callers decide what to emit.
fn build_module<'ctx>(
    context: &'ctx Context,
    inputs: &CodegenInputs<'_>,
) -> CompileResult<Module<'ctx>> {
    let CodegenInputs {
        functions,
        type_pool,
        strings,
        bytes,
        interner,
        interface_defs,
        interface_vtables,
    } = *inputs;
    let module = context.create_module("gruel_module");
    let builder = context.create_builder();

    // Create LLVM global constants for each string literal.
    let string_globals: Vec<GlobalValue<'_>> = strings
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let bytes = s.as_bytes();
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

    // Create LLVM global constants for each byte blob (`@embed_file`).
    // These live in the binary's read-only segment; `BytesConst` lowers to
    // a `Slice(u8)` aggregate `{ ptr, i64 len }` pointing at the global.
    // Empty blobs still emit a 1-byte placeholder global so the slice's
    // pointer is non-null; the slice's reported length comes from
    // `bytes_lens`, which is the *real* (unpadded) blob length.
    let bytes_lens: Vec<u64> = bytes.iter().map(|b| b.len() as u64).collect();
    let bytes_globals: Vec<GlobalValue<'_>> = bytes
        .iter()
        .enumerate()
        .map(|(i, blob)| {
            let raw: &[u8] = if blob.is_empty() {
                &[0u8]
            } else {
                blob.as_slice()
            };
            let array_val = context.const_string(raw, false);
            let global = module.add_global(
                context.i8_type().array_type(raw.len() as u32),
                None,
                &format!(".embed.{}", i),
            );
            global.set_constant(true);
            global.set_linkage(inkwell::module::Linkage::Private);
            global.set_initializer(&array_val);
            global
        })
        .collect();

    // Declare all functions first so that forward calls resolve.
    let mut declared: Vec<(&Cfg, FunctionValue<'_>)> = Vec::with_capacity(functions.len());
    for cfg in functions {
        let fn_value = declare_function(cfg, context, &module, type_pool)?;
        declared.push((cfg, fn_value));
    }

    // Build a name → FunctionValue map for call resolution.
    let fn_map: HashMap<&str, FunctionValue<'_>> = declared
        .iter()
        .map(|(cfg, fv)| (cfg.fn_name(), *fv))
        .collect();

    // ADR-0056 Phase 4d-extended: emit one vtable global per
    // `(StructId, InterfaceId)` pair. The vtable holds function pointers
    // for each interface method, in declaration order.
    let mut vtable_map: HashMap<(gruel_air::StructId, gruel_air::InterfaceId), GlobalValue<'ctx>> =
        HashMap::default();
    for (&(struct_id, interface_id), witness) in interface_vtables.iter() {
        let iface_def = &interface_defs[interface_id.0 as usize];
        let n_methods = iface_def.methods.len();
        let ptr_ty = context.ptr_type(inkwell::AddressSpace::default());
        let array_ty = ptr_ty.array_type(n_methods.max(1) as u32);

        // Resolve each interface slot to the conforming type's LLVM
        // function pointer.
        let mut slots: Vec<inkwell::values::PointerValue<'ctx>> = Vec::with_capacity(n_methods);
        if n_methods == 0 {
            // Empty interface → emit a single null slot so the array has
            // a non-zero length (LLVM can be picky about zero-length
            // global arrays).
            slots.push(ptr_ty.const_null());
        } else {
            for (i, req) in iface_def.methods.iter().enumerate() {
                let (_concrete_struct_id, _method_sym) = witness[i];
                // Function name: `StructName.method`.
                let struct_name = type_pool.struct_def(struct_id).name.clone();
                let mangled = format!("{}.{}", struct_name, req.name);
                let fn_ptr = match fn_map.get(mangled.as_str()) {
                    Some(fv) => fv.as_global_value().as_pointer_value(),
                    None => ptr_ty.const_null(),
                };
                slots.push(fn_ptr);
            }
        }

        let init = ptr_ty.const_array(&slots);
        let vtbl_name = format!("__vtable__s{}__i{}", struct_id.0, interface_id.0);
        let global = module.add_global(array_ty, None, &vtbl_name);
        global.set_constant(true);
        global.set_linkage(inkwell::module::Linkage::Private);
        global.set_initializer(&init);
        vtable_map.insert((struct_id, interface_id), global);
    }

    let mod_ctx = ModuleCtx {
        ctx: context,
        builder: &builder,
        module: &module,
        type_pool,
        interner,
        strings,
        string_globals: &string_globals,
        bytes_globals: &bytes_globals,
        bytes_lens: &bytes_lens,
        fn_map: &fn_map,
        vtable_map: &vtable_map,
        interface_defs,
    };

    // Define each function body.
    for (cfg, fn_value) in &declared {
        define_function(cfg, fn_value, &mod_ctx)?;
    }

    // Verify the module.
    module
        .verify()
        .map_err(|e| llvm_error(format!("LLVM module verification failed: {}", e)))?;

    Ok(module)
}

/// Map a Gruel `OptLevel` to the corresponding `inkwell::OptimizationLevel`.
fn to_llvm_opt_level(opt_level: OptLevel) -> OptimizationLevel {
    match opt_level {
        OptLevel::O0 => OptimizationLevel::None,
        OptLevel::O1 => OptimizationLevel::Less,
        OptLevel::O2 => OptimizationLevel::Default,
        OptLevel::O3 => OptimizationLevel::Aggressive,
    }
}

/// Run LLVM's mid-end optimization pipeline on the module for the given opt level.
///
/// For `-O0` this is a no-op. For `-O1+` it runs `default<OX>` which includes
/// InstCombine, GVN, SCCP, ADCE, SimplifyCFG, and more.
fn run_llvm_passes(
    module: &Module<'_>,
    target_machine: &TargetMachine,
    opt_level: OptLevel,
) -> CompileResult<()> {
    let passes = match opt_level {
        OptLevel::O0 => return Ok(()),
        OptLevel::O1 => "default<O1>",
        OptLevel::O2 => "default<O2>",
        OptLevel::O3 => "default<O3>",
    };
    module
        .run_passes(passes, target_machine, PassBuilderOptions::create())
        .map_err(|e| llvm_error(format!("LLVM pass pipeline failed: {}", e)))
}

/// Generate pre-optimization LLVM bitcode (ADR-0074 Phase 5).
///
/// Lowers all functions into a single LLVM module via [`build_module`] and
/// returns the bitcode bytes WITHOUT running the optimizer. Pairs with
/// [`compile_bitcode_to_object`] to round-trip through the cache.
pub fn generate_bitcode(inputs: &CodegenInputs<'_>) -> CompileResult<Vec<u8>> {
    let context = Context::create();
    let module = build_module(&context, inputs)?;
    let buffer = module.write_bitcode_to_memory();
    Ok(buffer.as_slice().to_vec())
}

/// Run the LLVM mid-end optimizer + back-end on cached or fresh bitcode
/// to produce a native object file (ADR-0074 Phase 5).
///
/// The bitcode is parsed into a fresh module, optimized at `opt_level`,
/// and emitted as object bytes. Cache hits skip [`generate_bitcode`]
/// entirely and call this directly.
pub fn compile_bitcode_to_object(bitcode: &[u8], opt_level: OptLevel) -> CompileResult<Vec<u8>> {
    LlvmTarget::initialize_native(&InitializationConfig::default())
        .map_err(|e| llvm_error(format!("LLVM target initialization failed: {}", e)))?;

    let context = Context::create();
    let buffer =
        inkwell::memory_buffer::MemoryBuffer::create_from_memory_range(bitcode, "cached_bitcode");
    let module = inkwell::module::Module::parse_bitcode_from_buffer(&buffer, &context)
        .map_err(|e| llvm_error(format!("failed to parse cached bitcode: {}", e)))?;

    let target_triple = TargetMachine::get_default_triple();
    let llvm_target = LlvmTarget::from_triple(&target_triple)
        .map_err(|e| llvm_error(format!("failed to get LLVM target: {}", e)))?;
    let target_machine = llvm_target
        .create_target_machine(
            &target_triple,
            "generic",
            "",
            to_llvm_opt_level(opt_level),
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or_else(|| llvm_error("failed to create LLVM TargetMachine"))?;

    run_llvm_passes(&module, &target_machine, opt_level)?;

    let obj_buf = target_machine
        .write_to_memory_buffer(&module, FileType::Object)
        .map_err(|e| llvm_error(format!("LLVM object emission failed: {}", e)))?;

    Ok(obj_buf.as_slice().to_vec())
}

/// Generate a native object file from a set of function CFGs.
///
/// All functions are lowered into a single LLVM module. The module is then
/// compiled to an in-memory object file buffer by the host machine's LLVM
/// code generator.
pub fn generate(inputs: &CodegenInputs<'_>, opt_level: OptLevel) -> CompileResult<Vec<u8>> {
    LlvmTarget::initialize_native(&InitializationConfig::default())
        .map_err(|e| llvm_error(format!("LLVM target initialization failed: {}", e)))?;

    let context = Context::create();
    let module = build_module(&context, inputs)?;
    let target_triple = TargetMachine::get_default_triple();
    let llvm_target = LlvmTarget::from_triple(&target_triple)
        .map_err(|e| llvm_error(format!("failed to get LLVM target: {}", e)))?;
    let target_machine = llvm_target
        .create_target_machine(
            &target_triple,
            "generic",
            "",
            to_llvm_opt_level(opt_level),
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or_else(|| llvm_error("failed to create LLVM TargetMachine"))?;

    // Run the mid-end optimization pipeline (no-op at O0).
    run_llvm_passes(&module, &target_machine, opt_level)?;

    let obj_buf = target_machine
        .write_to_memory_buffer(&module, FileType::Object)
        .map_err(|e| llvm_error(format!("LLVM object emission failed: {}", e)))?;

    Ok(obj_buf.as_slice().to_vec())
}

/// Generate LLVM textual IR (`*.ll` format) from a set of function CFGs.
///
/// Returns the human-readable LLVM IR as a string. This is used by
/// `--emit asm` to produce inspectable IR in place of native assembly.
/// At `-O1+` the IR is the post-optimization form.
pub fn generate_ir(inputs: &CodegenInputs<'_>, opt_level: OptLevel) -> CompileResult<String> {
    let context = Context::create();
    let module = build_module(&context, inputs)?;

    if opt_level != OptLevel::O0 {
        // generate_ir needs a TargetMachine to run passes. We do a lightweight
        // native-target init just for this purpose.
        LlvmTarget::initialize_native(&InitializationConfig::default())
            .map_err(|e| llvm_error(format!("LLVM target initialization failed: {}", e)))?;
        let target_triple = TargetMachine::get_default_triple();
        let llvm_target = LlvmTarget::from_triple(&target_triple)
            .map_err(|e| llvm_error(format!("failed to get LLVM target: {}", e)))?;
        let target_machine = llvm_target
            .create_target_machine(
                &target_triple,
                "generic",
                "",
                to_llvm_opt_level(opt_level),
                RelocMode::PIC,
                CodeModel::Default,
            )
            .ok_or_else(|| llvm_error("failed to create LLVM TargetMachine"))?;
        run_llvm_passes(&module, &target_machine, opt_level)?;
    }

    Ok(module.print_to_string().to_string())
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

    let f = module.add_function(name, fn_type, None);

    // Gruel has no exceptions/unwinding, so all functions are `nounwind`.
    f.add_attribute(
        inkwell::attributes::AttributeLoc::Function,
        ctx.create_enum_attribute(
            inkwell::attributes::Attribute::get_named_enum_kind_id("nounwind"),
            0,
        ),
    );

    // By-ref params (inout, borrow) get `noalias`: the language spec forbids
    // aliasing (check_exclusivity), so this is sound. Borrow params also get
    // `readonly` since the callee cannot mutate through them.
    let slot_to_llvm = build_slot_to_llvm_param(cfg, type_pool);
    let num_params = cfg.num_params() as usize;
    let mut i = 0usize;
    while i < num_params {
        if cfg.is_param_by_ref(i as u32) {
            let llvm_idx = slot_to_llvm[i];
            f.add_attribute(
                inkwell::attributes::AttributeLoc::Param(llvm_idx),
                ctx.create_enum_attribute(
                    inkwell::attributes::Attribute::get_named_enum_kind_id("noalias"),
                    0,
                ),
            );
            if cfg.is_param_borrow(i as u32) {
                f.add_attribute(
                    inkwell::attributes::AttributeLoc::Param(llvm_idx),
                    ctx.create_enum_attribute(
                        inkwell::attributes::Attribute::get_named_enum_kind_id("readonly"),
                        0,
                    ),
                );
            }
            i += 1;
        } else {
            let ty = cfg
                .param_type(i as u32)
                .expect("param slot in range must have a type");
            let raw_slot_count = type_pool.abi_slot_count(ty);
            i += raw_slot_count.max(1) as usize;
        }
    }

    Ok(f)
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
        if cfg.is_param_by_ref(i as u32) {
            // By-reference params are always opaque pointers in LLVM IR (1 slot).
            result.push(ctx.ptr_type(inkwell::AddressSpace::default()).into());
            i += 1;
        } else {
            // Value params: emit one LLVM param per Gruel param, skipping
            // the intermediate ABI slots of multi-slot composites.
            let ty = cfg
                .param_type(i as u32)
                .expect("param slot in range must have a type");
            let raw_slot_count = type_pool.abi_slot_count(ty);
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
        if cfg.is_param_by_ref(i as u32) {
            table[i] = llvm_idx;
            llvm_idx += 1;
            i += 1;
        } else {
            let ty = cfg
                .param_type(i as u32)
                .expect("param slot in range must have a type");
            let raw_slot_count = type_pool.abi_slot_count(ty);
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
    /// LLVM globals holding the raw bytes of each `@embed_file` blob.
    bytes_globals: &'a [GlobalValue<'ctx>],
    /// Real (unpadded) length of each `@embed_file` blob in bytes; parallel
    /// to `bytes_globals`. The slice's `len` field is built from this.
    bytes_lens: &'a [u64],
    /// Maps CFG block IDs to LLVM basic blocks.
    llvm_blocks: Vec<LlvmBlock<'ctx>>,
    /// Maps CFG value indices to LLVM values.
    values: Vec<Option<BasicValueEnum<'ctx>>>,
    /// Alloca slots for local variables (one per slot index).
    locals: Vec<Option<inkwell::values::PointerValue<'ctx>>>,
    /// Tracks local allocas for lifetime marker emission at returns.
    local_lifetime_ptrs: Vec<inkwell::values::PointerValue<'ctx>>,
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
    /// (StructId, InterfaceId) → vtable global. Populated once at module
    /// build time and shared across all functions.
    vtable_map: &'a HashMap<(gruel_air::StructId, gruel_air::InterfaceId), GlobalValue<'ctx>>,
    /// Interface definitions for vtable layout (slot count per interface).
    interface_defs: &'a [gruel_air::InterfaceDef],
}

impl<'ctx, 'a> FnCodegen<'ctx, 'a> {
    fn new(cfg: &'a Cfg, fn_value: FunctionValue<'ctx>, mod_ctx: &ModuleCtx<'ctx, 'a>) -> Self {
        let value_count = cfg.value_count();
        let num_locals = cfg.num_locals() as usize;

        // Create LLVM basic blocks for each CFG block.
        let llvm_blocks: Vec<LlvmBlock<'ctx>> = cfg
            .blocks()
            .iter()
            .map(|bb| {
                mod_ctx
                    .ctx
                    .append_basic_block(fn_value, &format!("bb{}", bb.id.as_u32()))
            })
            .collect();

        let num_params = cfg.num_params() as usize;
        let slot_to_llvm_param = build_slot_to_llvm_param(cfg, mod_ctx.type_pool);
        Self {
            cfg,
            fn_value,
            ctx: mod_ctx.ctx,
            builder: mod_ctx.builder,
            module: mod_ctx.module,
            type_pool: mod_ctx.type_pool,
            interner: mod_ctx.interner,
            fn_map: mod_ctx.fn_map,
            strings: mod_ctx.strings,
            string_globals: mod_ctx.string_globals,
            bytes_globals: mod_ctx.bytes_globals,
            bytes_lens: mod_ctx.bytes_lens,
            llvm_blocks,
            values: vec![None; value_count],
            locals: vec![None; num_locals],
            local_lifetime_ptrs: Vec::new(),
            phi_nodes: vec![None; value_count],
            param_allocas: vec![None; num_params],
            slot_to_llvm_param,
            vtable_map: mod_ctx.vtable_map,
            interface_defs: mod_ctx.interface_defs,
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
        let ptr = self
            .builder
            .build_alloca(llvm_ty, &format!("slot{}", slot))
            .expect("build_alloca failed");

        // Emit llvm.lifetime.start immediately after the alloca.
        let lifetime_start_fn = self.get_or_declare_lifetime_start();
        self.builder
            .build_call(lifetime_start_fn, &[ptr.into()], "")
            .unwrap();

        // Track for lifetime.end emission at returns.
        self.local_lifetime_ptrs.push(ptr);

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
        let fn_type = self
            .ctx
            .void_type()
            .fn_type(&[self.ctx.i32_type().into()], false);
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
        // Mark as `cold` so LLVM deprioritizes panic paths in code layout and inlining.
        f.add_attribute(
            inkwell::attributes::AttributeLoc::Function,
            self.ctx.create_string_attribute("cold", ""),
        );
        f
    }

    /// Wrap a boolean with `llvm.expect.i1(val, expected)` to hint branch prediction.
    ///
    /// Returns a new i1 value with the expectation metadata attached.
    fn build_expect_i1(
        &self,
        val: inkwell::values::IntValue<'ctx>,
        expected: bool,
    ) -> inkwell::values::IntValue<'ctx> {
        let expect_intrinsic =
            Intrinsic::find("llvm.expect").expect("llvm.expect intrinsic not found");
        let bool_ty = self.ctx.bool_type();
        let expect_fn = expect_intrinsic
            .get_declaration(self.module, &[bool_ty.into()])
            .expect("failed to declare llvm.expect.i1");
        let expected_val = bool_ty.const_int(expected as u64, false);
        self.builder
            .build_call(expect_fn, &[val.into(), expected_val.into()], "exp")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_int_value()
    }

    /// Get or declare `llvm.lifetime.start.p0(ptr) -> void`.
    fn get_or_declare_lifetime_start(&self) -> FunctionValue<'ctx> {
        let intrinsic = Intrinsic::find("llvm.lifetime.start")
            .expect("llvm.lifetime.start intrinsic not found");
        let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
        intrinsic
            .get_declaration(self.module, &[ptr_ty.into()])
            .expect("failed to declare llvm.lifetime.start")
    }

    /// Get or declare `llvm.lifetime.end.p0(ptr) -> void`.
    fn get_or_declare_lifetime_end(&self) -> FunctionValue<'ctx> {
        let intrinsic =
            Intrinsic::find("llvm.lifetime.end").expect("llvm.lifetime.end intrinsic not found");
        let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
        intrinsic
            .get_declaration(self.module, &[ptr_ty.into()])
            .expect("failed to declare llvm.lifetime.end")
    }

    /// Emit `llvm.lifetime.end` for all tracked local allocas.
    ///
    /// Called before function return terminators so LLVM can reuse stack slots.
    fn emit_lifetime_ends(&self) {
        if self.local_lifetime_ptrs.is_empty() {
            return;
        }
        let lifetime_end_fn = self.get_or_declare_lifetime_end();
        for &ptr in &self.local_lifetime_ptrs {
            self.builder
                .build_call(lifetime_end_fn, &[ptr.into()], "")
                .unwrap();
        }
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
    /// ADR-0072: String layout is `{ bytes: Vec(u8) }` with the inner
    /// Vec(u8) being `{ ptr, i64 len, i64 cap }`. The pointer is stored
    /// as a typed `ptr` (no inttoptr needed).
    fn extract_str_ptr_len(
        &mut self,
        str_val: BasicValueEnum<'ctx>,
    ) -> (
        inkwell::values::PointerValue<'ctx>,
        inkwell::values::IntValue<'ctx>,
    ) {
        let sv = str_val.into_struct_value();
        let outer: AggregateValueEnum<'ctx> = sv.into();
        let inner = self
            .builder
            .build_extract_value(outer, 0, "str_bytes")
            .expect("extract String.bytes")
            .into_struct_value();
        let inner_agg: AggregateValueEnum<'ctx> = inner.into();
        let ptr = self
            .builder
            .build_extract_value(inner_agg, 0, "str_ptr")
            .expect("extract bytes.ptr")
            .into_pointer_value();
        let inner_agg: AggregateValueEnum<'ctx> = inner.into();
        let len = self
            .builder
            .build_extract_value(inner_agg, 1, "str_len")
            .expect("extract bytes.len")
            .into_int_value();
        (ptr, len)
    }

    /// Extract the `(ptr, len, cap)` fields from a String struct value.
    ///
    /// Same as [`extract_str_ptr_len`] but also returns the `cap` field.
    fn extract_str_ptr_len_cap(
        &mut self,
        str_val: BasicValueEnum<'ctx>,
    ) -> (
        inkwell::values::PointerValue<'ctx>,
        inkwell::values::IntValue<'ctx>,
        inkwell::values::IntValue<'ctx>,
    ) {
        let sv = str_val.into_struct_value();
        let outer: AggregateValueEnum<'ctx> = sv.into();
        let inner = self
            .builder
            .build_extract_value(outer, 0, "str_bytes")
            .expect("extract String.bytes")
            .into_struct_value();
        let inner_agg: AggregateValueEnum<'ctx> = inner.into();
        let ptr = self
            .builder
            .build_extract_value(inner_agg, 0, "str_ptr")
            .expect("extract bytes.ptr")
            .into_pointer_value();
        let inner_agg: AggregateValueEnum<'ctx> = inner.into();
        let len = self
            .builder
            .build_extract_value(inner_agg, 1, "str_len")
            .expect("extract bytes.len")
            .into_int_value();
        let inner_agg: AggregateValueEnum<'ctx> = inner.into();
        let cap = self
            .builder
            .build_extract_value(inner_agg, 2, "str_cap")
            .expect("extract bytes.cap")
            .into_int_value();
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

    /// Get or declare `__gruel_str_cmp(ptr, len, ptr, len) -> i8`.
    fn get_or_declare_str_cmp(&self) -> FunctionValue<'ctx> {
        const NAME: &str = "__gruel_str_cmp";
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

    /// Build a string ordering comparison using `__gruel_str_cmp`.
    /// `pred` should be the IntPredicate for comparing the cmp result against 0.
    fn build_str_cmp(
        &mut self,
        l: BasicValueEnum<'ctx>,
        r: BasicValueEnum<'ctx>,
        pred: IntPredicate,
        name: &str,
    ) -> inkwell::values::IntValue<'ctx> {
        let (ptr1, len1) = self.extract_str_ptr_len(l);
        let (ptr2, len2) = self.extract_str_ptr_len(r);
        let str_cmp_fn = self.get_or_declare_str_cmp();
        let result = self
            .builder
            .build_call(
                str_cmp_fn,
                &[ptr1.into(), len1.into(), ptr2.into(), len2.into()],
                "strcmp",
            )
            .unwrap();
        let cmp_val = result
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_int_value();
        let zero = self.ctx.i8_type().const_zero();
        self.builder
            .build_int_compare(pred, cmp_val, zero, name)
            .unwrap()
    }

    /// Get or declare `__gruel_drop_String(ptr, len, cap) -> void`.
    fn get_or_declare_drop_string(&self) -> FunctionValue<'ctx> {
        const NAME: &str = "__gruel_drop_String";
        if let Some(f) = self.module.get_function(NAME) {
            return f;
        }
        let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.ctx.i64_type();
        let fn_type = self
            .ctx
            .void_type()
            .fn_type(&[ptr_ty.into(), i64_ty.into(), i64_ty.into()], false);
        self.module.add_function(NAME, fn_type, None)
    }

    /// Extract the LLVM fields of a struct or elements of an array as a flat `Vec`.
    ///
    /// Used to build the argument list for synthesized `__gruel_drop_*` functions,
    /// which take each non-void field / element as a separate LLVM parameter.
    fn extract_fields_for_drop(
        &mut self,
        val: BasicValueEnum<'ctx>,
        ty: Type,
    ) -> Vec<BasicValueEnum<'ctx>> {
        match ty.kind() {
            TypeKind::Struct(id) => {
                let def = self.type_pool.struct_def(id);
                let sv = val.into_struct_value();
                let mut args = Vec::new();
                let mut llvm_idx = 0u32;
                for field in &def.fields {
                    if gruel_type_to_llvm(field.ty, self.ctx, self.type_pool).is_some() {
                        let agg: AggregateValueEnum<'ctx> = sv.into();
                        let fv = self
                            .builder
                            .build_extract_value(agg, llvm_idx, "df")
                            .expect("extract struct field for drop");
                        args.push(fv);
                        llvm_idx += 1;
                    }
                }
                args
            }
            TypeKind::Array(id) => {
                let (_, len) = self.type_pool.array_def(id);
                let av = val.into_array_value();
                let mut args = Vec::new();
                for i in 0..len as u32 {
                    let agg: AggregateValueEnum<'ctx> = av.into();
                    let ev = self
                        .builder
                        .build_extract_value(agg, i, "de")
                        .expect("extract array element for drop");
                    args.push(ev);
                }
                args
            }
            _ => vec![],
        }
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
        let ptr = self
            .builder
            .build_alloca(ty, name)
            .expect("build_alloca failed");
        if let Some(bb) = current_bb {
            self.builder.position_at_end(bb);
        }
        ptr
    }

    /// Write `value` (interpreted as a little-endian unsigned integer of
    /// `width` bytes) into `slot[offset..offset+width]` (ADR-0069).
    ///
    /// Used by niche-encoded enums to encode the unit variant and to read the
    /// niche bytes during match dispatch.
    fn write_niche_bytes(
        &mut self,
        slot: inkwell::values::PointerValue<'ctx>,
        storage_ty: inkwell::types::ArrayType<'ctx>,
        offset: u32,
        width: u8,
        value: u128,
    ) {
        let zero = self.ctx.i64_type().const_zero();
        let off = self.ctx.i64_type().const_int(offset as u64, false);
        let ptr = unsafe {
            self.builder
                .build_gep(storage_ty, slot, &[zero, off], "niche_ptr")
                .expect("build_gep failed")
        };
        // Use an integer of the appropriate bit width and store little-endian
        // (LLVM's default storage on the supported targets).
        let int_ty = niche_int_type(self.ctx, width);
        let int_val = int_ty.const_int(value as u64, false);
        let store = self.builder.build_store(ptr, int_val).unwrap();
        store.set_alignment(1).unwrap();
    }

    /// Load `width` bytes at `slot[offset..offset+width]` as an unsigned
    /// integer (ADR-0069).
    fn load_niche_bytes(
        &mut self,
        slot: inkwell::values::PointerValue<'ctx>,
        storage_ty: inkwell::types::ArrayType<'ctx>,
        offset: u32,
        width: u8,
    ) -> inkwell::values::IntValue<'ctx> {
        let zero = self.ctx.i64_type().const_zero();
        let off = self.ctx.i64_type().const_int(offset as u64, false);
        let ptr = unsafe {
            self.builder
                .build_gep(storage_ty, slot, &[zero, off], "niche_ptr")
                .expect("build_gep failed")
        };
        let int_ty = niche_int_type(self.ctx, width);
        let load = self.builder.build_load(int_ty, ptr, "niche_val").unwrap();
        load.as_instruction_value()
            .unwrap()
            .set_alignment(1)
            .unwrap();
        load.into_int_value()
    }

    /// Spill a niche-encoded enum value to a stack slot, load the niche bytes,
    /// and synthesize an integer discriminant of `discrim_ty`'s width:
    /// `niche_value` → `unit_variant`, anything else → `data_variant`
    /// (ADR-0069).
    #[allow(clippy::too_many_arguments)]
    fn synthesize_niche_discriminant(
        &mut self,
        raw_val: BasicValueEnum<'ctx>,
        storage_size: u64,
        niche_offset: u32,
        niche_width: u8,
        niche_value: u128,
        unit_variant: u32,
        data_variant: u32,
        discrim_ty: Type,
    ) -> inkwell::values::IntValue<'ctx> {
        let storage_ty = self.ctx.i8_type().array_type(storage_size as u32);
        let slot = self.build_entry_alloca(storage_ty.into(), "niche_enum_slot");
        self.builder.build_store(slot, raw_val).unwrap();
        let niche_bytes = self.load_niche_bytes(slot, storage_ty, niche_offset, niche_width);
        let niche_const =
            niche_int_type(self.ctx, niche_width).const_int(niche_value as u64, false);
        let is_unit = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                niche_bytes,
                niche_const,
                "is_unit",
            )
            .unwrap();
        let discrim_llvm_ty = gruel_type_to_llvm(discrim_ty, self.ctx, self.type_pool)
            .expect("enum discriminant type must have an LLVM type")
            .into_int_type();
        let unit_const = discrim_llvm_ty.const_int(unit_variant as u64, false);
        let data_const = discrim_llvm_ty.const_int(data_variant as u64, false);
        self.builder
            .build_select(is_unit, unit_const, data_const, "niche_disc")
            .unwrap()
            .into_int_value()
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
            self.builder
                .build_int_z_extend(index, i64_ty, "bidx")
                .unwrap()
        } else if bits > 64 {
            self.builder
                .build_int_truncate(index, i64_ty, "bidx")
                .unwrap()
        } else {
            index
        };
        let len_val = i64_ty.const_int(length, false);
        let in_bounds = self
            .builder
            .build_int_compare(IntPredicate::ULT, idx_i64, len_val, "bchk")
            .unwrap();
        let in_bounds = self.build_expect_i1(in_bounds, true);

        let current_fn = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_parent()
            .unwrap();
        let ok_bb = self.ctx.append_basic_block(current_fn, "binbounds");
        let oob_bb = self.ctx.append_basic_block(current_fn, "boob");
        self.builder
            .build_conditional_branch(in_bounds, ok_bb, oob_bb)
            .unwrap();

        // Out-of-bounds handler: call __gruel_bounds_check() then unreachable.
        self.builder.position_at_end(oob_bb);
        let check_fn = self.get_or_declare_noreturn_fn("__gruel_bounds_check");
        self.builder.build_call(check_fn, &[], "").unwrap();
        self.builder.build_unreachable().unwrap();

        // Continue in the in-bounds block.
        self.builder.position_at_end(ok_bb);
    }

    /// Zero-extend or truncate `index` to `i64` for use in GEP instructions.
    fn index_to_i64(
        &self,
        index: inkwell::values::IntValue<'ctx>,
    ) -> inkwell::values::IntValue<'ctx> {
        let i64_ty = self.ctx.i64_type();
        let bits = index.get_type().get_bit_width();
        if bits < 64 {
            self.builder
                .build_int_z_extend(index, i64_ty, "iidx")
                .unwrap()
        } else if bits > 64 {
            self.builder
                .build_int_truncate(index, i64_ty, "iidx")
                .unwrap()
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
        let ptr = self
            .builder
            .build_alloca(llvm_ty, &format!("pslot{}", slot))
            .expect("build_alloca failed");
        if let Some(bb) = current_bb {
            self.builder.position_at_end(bb);
        }

        // Spill the fn param value into the alloca so GEP can address into it.
        let llvm_param_idx = self.slot_to_llvm_param[slot];
        let param_val = self
            .fn_value
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
                    if self.cfg.is_param_by_ref(param_slot) {
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
                if self.cfg.is_param_by_ref(param_slot) {
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
                Projection::Field {
                    struct_id,
                    field_index,
                } => {
                    let llvm_idx = self.gruel_to_llvm_field_index(*struct_id, *field_index);
                    let struct_llvm_ty = gruel_type_to_llvm(current_ty, self.ctx, self.type_pool)?
                        .into_struct_type();
                    current_ptr = self
                        .builder
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

        let call = self
            .builder
            .build_call(intrinsic_fn, &[l.into(), r.into()], "ovf")
            .unwrap();
        let struct_val = call
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_struct_value();
        let result = self
            .builder
            .build_extract_value(struct_val, 0, "res")
            .unwrap()
            .into_int_value();
        let overflow = self
            .builder
            .build_extract_value(struct_val, 1, "ovf_flag")
            .unwrap()
            .into_int_value();
        let overflow = self.build_expect_i1(overflow, false);

        // Emit conditional branch to overflow handler or continuation.
        let current_fn = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_parent()
            .unwrap();
        let overflow_bb = self.ctx.append_basic_block(current_fn, "ovf_handler");
        let cont_bb = self.ctx.append_basic_block(current_fn, "ovf_cont");

        self.builder
            .build_conditional_branch(overflow, overflow_bb, cont_bb)
            .unwrap();

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
        let is_zero = self
            .builder
            .build_int_compare(IntPredicate::EQ, divisor, zero, "divzero_check")
            .unwrap();
        let is_zero = self.build_expect_i1(is_zero, false);

        let current_fn = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_parent()
            .unwrap();
        let zero_bb = self.ctx.append_basic_block(current_fn, "divzero_handler");
        let cont_bb = self.ctx.append_basic_block(current_fn, "divzero_cont");

        self.builder
            .build_conditional_branch(is_zero, zero_bb, cont_bb)
            .unwrap();

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
                    let result = self
                        .builder
                        .build_call(
                            str_eq_fn,
                            &[ptr1.into(), len1.into(), ptr2.into(), len2.into()],
                            "streq",
                        )
                        .unwrap();
                    let byte_val = result
                        .try_as_basic_value()
                        .basic()
                        .unwrap()
                        .into_int_value();
                    // __gruel_str_eq returns i8; convert to i1 for use as a bool.
                    let zero = self.ctx.i8_type().const_zero();
                    return self
                        .builder
                        .build_int_compare(IntPredicate::NE, byte_val, zero, "streq_b")
                        .unwrap();
                }
                let mut all_eq = self.ctx.bool_type().const_int(1, false); // start true
                let mut llvm_idx = 0u32;
                for field in &struct_def.fields {
                    if gruel_type_to_llvm(field.ty, self.ctx, self.type_pool).is_none() {
                        continue; // skip void fields
                    }
                    let l_agg: AggregateValueEnum<'ctx> = l.into_struct_value().into();
                    let r_agg: AggregateValueEnum<'ctx> = r.into_struct_value().into();
                    let l_field = self
                        .builder
                        .build_extract_value(l_agg, llvm_idx, "l_f")
                        .expect("build_extract_value failed");
                    let r_field = self
                        .builder
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
                    let l_elem = self
                        .builder
                        .build_extract_value(l_agg, i, "l_e")
                        .expect("build_extract_value failed");
                    let r_elem = self
                        .builder
                        .build_extract_value(r_agg, i, "r_e")
                        .expect("build_extract_value failed");
                    let elem_eq = self.build_value_eq(elem_ty, l_elem, r_elem);
                    all_eq = self.builder.build_and(all_eq, elem_eq, "and_eq").unwrap();
                }
                all_eq
            }
            _ if ty.is_float() => {
                let l_f = l.into_float_value();
                let r_f = r.into_float_value();
                self.builder
                    .build_float_compare(FloatPredicate::OEQ, l_f, r_f, "feq")
                    .unwrap()
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
                let phi = self
                    .builder
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

    /// Lower a `CfgInstData::Bin` into an LLVM value. Internal cases mirror
    /// the prior per-variant codegen (float vs. int paths, signed vs.
    /// unsigned, string compare, overflow-checked arithmetic).
    fn translate_bin(&mut self, op: BinOp, lhs: CfgValue, rhs: CfgValue) -> BasicValueEnum<'ctx> {
        let lhs_ty = self.cfg.get_inst(lhs).ty;
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                if lhs_ty.is_float() {
                    let l = self.get_value(lhs).into_float_value();
                    let r = self.get_value(rhs).into_float_value();
                    match op {
                        BinOp::Add => self.builder.build_float_add(l, r, "fadd").unwrap().into(),
                        BinOp::Sub => self.builder.build_float_sub(l, r, "fsub").unwrap().into(),
                        BinOp::Mul => self.builder.build_float_mul(l, r, "fmul").unwrap().into(),
                        _ => unreachable!(),
                    }
                } else {
                    let l = self.get_value(lhs).into_int_value();
                    let r = self.get_value(rhs).into_int_value();
                    let signed = is_signed_type(lhs_ty);
                    let intrinsic = match (op, signed) {
                        (BinOp::Add, true) => "llvm.sadd.with.overflow",
                        (BinOp::Add, false) => "llvm.uadd.with.overflow",
                        (BinOp::Sub, true) => "llvm.ssub.with.overflow",
                        (BinOp::Sub, false) => "llvm.usub.with.overflow",
                        (BinOp::Mul, true) => "llvm.smul.with.overflow",
                        (BinOp::Mul, false) => "llvm.umul.with.overflow",
                        _ => unreachable!(),
                    };
                    self.build_checked_int_op(l, r, intrinsic).into()
                }
            }
            BinOp::Div | BinOp::Mod => {
                if lhs_ty.is_float() {
                    let l = self.get_value(lhs).into_float_value();
                    let r = self.get_value(rhs).into_float_value();
                    if matches!(op, BinOp::Div) {
                        self.builder.build_float_div(l, r, "fdiv").unwrap().into()
                    } else {
                        self.builder.build_float_rem(l, r, "frem").unwrap().into()
                    }
                } else {
                    let l = self.get_value(lhs).into_int_value();
                    let r = self.get_value(rhs).into_int_value();
                    self.build_div_zero_check(r);
                    let signed = is_signed_type(lhs_ty);
                    let v = match (op, signed) {
                        (BinOp::Div, true) => {
                            self.builder.build_int_signed_div(l, r, "div").unwrap()
                        }
                        (BinOp::Div, false) => {
                            self.builder.build_int_unsigned_div(l, r, "div").unwrap()
                        }
                        (BinOp::Mod, true) => {
                            self.builder.build_int_signed_rem(l, r, "rem").unwrap()
                        }
                        (BinOp::Mod, false) => {
                            self.builder.build_int_unsigned_rem(l, r, "rem").unwrap()
                        }
                        _ => unreachable!(),
                    };
                    v.into()
                }
            }
            BinOp::Eq => {
                if gruel_type_to_llvm(lhs_ty, self.ctx, self.type_pool).is_none() {
                    // Unit == Unit is always true.
                    self.ctx.bool_type().const_int(1, false).into()
                } else {
                    let l = self.get_value(lhs);
                    let r = self.get_value(rhs);
                    self.build_value_eq(lhs_ty, l, r).into()
                }
            }
            BinOp::Ne => {
                if gruel_type_to_llvm(lhs_ty, self.ctx, self.type_pool).is_none() {
                    self.ctx.bool_type().const_int(0, false).into()
                } else {
                    let l = self.get_value(lhs);
                    let r = self.get_value(rhs);
                    let eq = self.build_value_eq(lhs_ty, l, r);
                    self.builder.build_not(eq, "ne").unwrap().into()
                }
            }
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                let (sint, uint, fpred, name) = match op {
                    BinOp::Lt => (
                        IntPredicate::SLT,
                        IntPredicate::ULT,
                        FloatPredicate::OLT,
                        "lt",
                    ),
                    BinOp::Gt => (
                        IntPredicate::SGT,
                        IntPredicate::UGT,
                        FloatPredicate::OGT,
                        "gt",
                    ),
                    BinOp::Le => (
                        IntPredicate::SLE,
                        IntPredicate::ULE,
                        FloatPredicate::OLE,
                        "le",
                    ),
                    BinOp::Ge => (
                        IntPredicate::SGE,
                        IntPredicate::UGE,
                        FloatPredicate::OGE,
                        "ge",
                    ),
                    _ => unreachable!(),
                };
                if self.is_builtin_string(lhs_ty) {
                    let l = self.get_value(lhs);
                    let r = self.get_value(rhs);
                    self.build_str_cmp(l, r, sint, &format!("str{}", name))
                        .into()
                } else if lhs_ty.is_float() {
                    let l = self.get_value(lhs).into_float_value();
                    let r = self.get_value(rhs).into_float_value();
                    self.builder
                        .build_float_compare(fpred, l, r, &format!("f{}", name))
                        .unwrap()
                        .into()
                } else {
                    let l = self.get_value(lhs).into_int_value();
                    let r = self.get_value(rhs).into_int_value();
                    let p = if is_signed_type(lhs_ty) { sint } else { uint };
                    self.builder
                        .build_int_compare(p, l, r, name)
                        .unwrap()
                        .into()
                }
            }
            BinOp::BitAnd => {
                let l = self.get_value(lhs).into_int_value();
                let r = self.get_value(rhs).into_int_value();
                self.builder.build_and(l, r, "and").unwrap().into()
            }
            BinOp::BitOr => {
                let l = self.get_value(lhs).into_int_value();
                let r = self.get_value(rhs).into_int_value();
                self.builder.build_or(l, r, "or").unwrap().into()
            }
            BinOp::BitXor => {
                let l = self.get_value(lhs).into_int_value();
                let r = self.get_value(rhs).into_int_value();
                self.builder.build_xor(l, r, "xor").unwrap().into()
            }
            BinOp::Shl => {
                let l = self.get_value(lhs).into_int_value();
                let r = self.get_value(rhs).into_int_value();
                self.builder.build_left_shift(l, r, "shl").unwrap().into()
            }
            BinOp::Shr => {
                let l = self.get_value(lhs).into_int_value();
                let r = self.get_value(rhs).into_int_value();
                let signed = is_signed_type(lhs_ty);
                self.builder
                    .build_right_shift(l, r, signed, "shr")
                    .unwrap()
                    .into()
            }
            BinOp::And | BinOp::Or => {
                // Short-circuit ops are lowered to control flow during CFG
                // construction; they should never reach codegen.
                unreachable!("logical {:?} should have been lowered to control flow", op)
            }
        }
    }

    /// Lower a `CfgInstData::Unary` into an LLVM value.
    fn translate_unary(&mut self, op: UnaryOp, operand: CfgValue) -> BasicValueEnum<'ctx> {
        match op {
            UnaryOp::Neg => {
                let op_ty = self.cfg.get_inst(operand).ty;
                if op_ty.is_float() {
                    let v = self.get_value(operand).into_float_value();
                    self.builder.build_float_neg(v, "fneg").unwrap().into()
                } else {
                    let v = self.get_value(operand).into_int_value();
                    let zero = v.get_type().const_zero();
                    self.build_checked_int_op(zero, v, "llvm.ssub.with.overflow")
                        .into()
                }
            }
            UnaryOp::Not => {
                let v = self.get_value(operand).into_int_value();
                let zero = v.get_type().const_zero();
                self.builder
                    .build_int_compare(IntPredicate::EQ, v, zero, "not")
                    .unwrap()
                    .into()
            }
            UnaryOp::BitNot => {
                let v = self.get_value(operand).into_int_value();
                self.builder.build_not(v, "bitnot").unwrap().into()
            }
        }
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

            CfgInstData::FloatConst(bits) => {
                let f64_val = f64::from_bits(bits);
                let llvm_ty = gruel_type_to_llvm(ty, self.ctx, self.type_pool)
                    .expect("float type must have LLVM representation");
                Some(llvm_ty.into_float_type().const_float(f64_val).into())
            }

            CfgInstData::BoolConst(b) => {
                Some(self.ctx.bool_type().const_int(b as u64, false).into())
            }

            CfgInstData::StringConst(idx) => {
                // ADR-0072: String is a newtype `{ bytes: Vec(u8) }`. Its
                // LLVM type is `{ {ptr, i64, i64} }` — an outer struct
                // wrapping the Vec(u8) aggregate. Build the inner Vec(u8)
                // first, then wrap it.
                let idx = idx as usize;
                let str_len = self.strings.get(idx).map(|s| s.len()).unwrap_or(0) as u64;
                let global = self.string_globals.get(idx);
                let i64_ty = self.ctx.i64_type();
                let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
                let data_ptr: inkwell::values::PointerValue<'ctx> = if let Some(g) = global {
                    g.as_pointer_value()
                } else {
                    ptr_ty.const_null()
                };
                let len_val = i64_ty.const_int(str_len, false);
                let cap_val = i64_ty.const_zero();
                // Build the Vec(u8) aggregate { ptr, len, cap }.
                let vec_agg = self.vec_agg_type();
                let undef_vec: AggregateValueEnum<'ctx> = vec_agg.get_undef().into();
                let with_p = self
                    .builder
                    .build_insert_value(undef_vec, data_ptr, 0, "sc_vec_p")
                    .unwrap();
                let with_l = self
                    .builder
                    .build_insert_value(with_p, len_val, 1, "sc_vec_l")
                    .unwrap();
                let inner_vec = self
                    .builder
                    .build_insert_value(with_l, cap_val, 2, "sc_vec_c")
                    .unwrap();
                // Wrap in the String outer struct { Vec(u8) }.
                let str_llvm_ty = gruel_type_to_llvm(ty, self.ctx, self.type_pool)
                    .expect("String type must have LLVM representation")
                    .into_struct_type();
                let undef_str: AggregateValueEnum<'ctx> = str_llvm_ty.get_undef().into();
                let agg = self
                    .builder
                    .build_insert_value(undef_str, inner_vec, 0, "sc_string")
                    .unwrap();
                Some(agg.as_basic_value_enum())
            }

            CfgInstData::BytesConst(idx) => {
                // Build a `Slice(u8)` aggregate `{ ptr, i64 len }` pointing
                // at the LLVM global that holds the embedded bytes.
                let idx = idx as usize;
                let len_value = self.bytes_lens.get(idx).copied().unwrap_or(0);
                let i64_ty = self.ctx.i64_type();
                let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
                let data_ptr: inkwell::values::PointerValue<'ctx> =
                    if let Some(g) = self.bytes_globals.get(idx) {
                        g.as_pointer_value()
                    } else {
                        ptr_ty.const_null()
                    };
                let slice_llvm_ty = gruel_type_to_llvm(ty, self.ctx, self.type_pool)
                    .expect("Slice(u8) has LLVM lowering")
                    .into_struct_type();
                let undef = slice_llvm_ty.get_undef();
                let with_ptr = self
                    .builder
                    .build_insert_value(undef, data_ptr, 0, "embed_p")
                    .unwrap();
                let agg = self
                    .builder
                    .build_insert_value(with_ptr, i64_ty.const_int(len_value, false), 1, "embed_s")
                    .unwrap();
                Some(agg.as_basic_value_enum())
            }

            CfgInstData::Param { index } => {
                let llvm_idx = self.slot_to_llvm_param[index as usize];
                let param_val = self
                    .fn_value
                    .get_nth_param(llvm_idx)
                    .expect("param index out of range");
                if self.cfg.is_param_by_ref(index) {
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

            CfgInstData::Bin(op, lhs, rhs) => Some(self.translate_bin(op, lhs, rhs)),
            CfgInstData::Unary(op, operand) => Some(self.translate_unary(op, operand)),

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
                        let ptr =
                            self.locals[slot as usize].expect("Load before Alloc — invalid CFG");
                        Some(self.builder.build_load(llvm_ty, ptr, "load").unwrap())
                    }
                }
            }
            // ADR-0062 / ADR-0063: `&x` / `&mut x` lower to the storage
            // pointer of the operand place. For a plain local we return the
            // alloca; for a field / index path we GEP into the local.
            CfgInstData::MakeRef { place, is_mut: _ } => {
                let elem_ty = ty;
                let inner_ty = match elem_ty.kind() {
                    gruel_air::TypeKind::Ref(id) => self.type_pool.ref_def(id),
                    gruel_air::TypeKind::MutRef(id) => self.type_pool.mut_ref_def(id),
                    _ => elem_ty,
                };
                self.build_place_gep_chain(&place, inner_ty).map(Into::into)
            }
            // ADR-0064: build a fat pointer `{ptr, len}` over a sub-range
            // of an array place. The base array's storage pointer is
            // recovered from the place via `build_place_gep_chain`. `lo`
            // defaults to 0 and `hi` defaults to `array_len`. We runtime
            // check `lo <= hi <= array_len` and panic via
            // `__gruel_bounds_check` on failure.
            CfgInstData::MakeSlice(data) => {
                let place = data.place;
                let array_len = data.array_len;
                let lo = data.lo;
                let hi = data.hi;
                let vec_base = data.vec_base;
                use inkwell::IntPredicate;

                let elem_ty = match ty.kind() {
                    gruel_air::TypeKind::Slice(id) => self.type_pool.slice_def(id),
                    gruel_air::TypeKind::MutSlice(id) => self.type_pool.mut_slice_def(id),
                    _ => unreachable!("MakeSlice produces a slice type"),
                };
                let i64_ty = self.ctx.i64_type();
                // For Vec bases, the "array length" is the runtime `len`
                // field. Read it now so the bounds check below works.
                let array_len_val = if vec_base {
                    let agg_ty = self.vec_agg_type();
                    let vec_ptr = match self.build_place_gep_chain(&place, ty) {
                        Some(p) => p,
                        None => return Ok(()),
                    };
                    let len_ptr = self
                        .builder
                        .build_struct_gep(agg_ty, vec_ptr, 1, "vec_slice_len")
                        .unwrap();
                    self.builder
                        .build_load(i64_ty, len_ptr, "vec_slice_len_val")
                        .unwrap()
                        .into_int_value()
                } else {
                    i64_ty.const_int(array_len, false)
                };

                let coerce_to_i64 = |this: &Self,
                                     v: inkwell::values::IntValue<'ctx>|
                 -> inkwell::values::IntValue<'ctx> {
                    let bits = v.get_type().get_bit_width();
                    if bits < 64 {
                        this.builder.build_int_z_extend(v, i64_ty, "slc").unwrap()
                    } else if bits > 64 {
                        this.builder.build_int_truncate(v, i64_ty, "slc").unwrap()
                    } else {
                        v
                    }
                };

                let lo_val = match lo {
                    Some(lo) => {
                        let v = self.get_value(lo).into_int_value();
                        coerce_to_i64(self, v)
                    }
                    None => i64_ty.const_zero(),
                };
                let hi_val = match hi {
                    Some(hi) => {
                        let v = self.get_value(hi).into_int_value();
                        coerce_to_i64(self, v)
                    }
                    None => array_len_val,
                };

                // Runtime check: lo <= hi && hi <= array_len.
                let lo_ok = self
                    .builder
                    .build_int_compare(IntPredicate::ULE, lo_val, hi_val, "slc_lo_hi")
                    .unwrap();
                let hi_ok = self
                    .builder
                    .build_int_compare(IntPredicate::ULE, hi_val, array_len_val, "slc_hi_n")
                    .unwrap();
                let in_bounds = self.builder.build_and(lo_ok, hi_ok, "slc_ok").unwrap();
                let in_bounds = self.build_expect_i1(in_bounds, true);
                let current_fn = self
                    .builder
                    .get_insert_block()
                    .unwrap()
                    .get_parent()
                    .unwrap();
                let ok_bb = self.ctx.append_basic_block(current_fn, "slc_ok");
                let oob_bb = self.ctx.append_basic_block(current_fn, "slc_oob");
                self.builder
                    .build_conditional_branch(in_bounds, ok_bb, oob_bb)
                    .unwrap();
                self.builder.position_at_end(oob_bb);
                let check_fn = self.get_or_declare_noreturn_fn("__gruel_bounds_check");
                self.builder.build_call(check_fn, &[], "").unwrap();
                self.builder.build_unreachable().unwrap();
                self.builder.position_at_end(ok_bb);

                // ptr = GEP &arr[lo]; offset by `lo` elements.
                // For Vec bases, the buffer is at field 0 of the aggregate.
                let arr_ptr = if vec_base {
                    let agg_ty = self.vec_agg_type();
                    let vec_ptr = match self.build_place_gep_chain(&place, ty) {
                        Some(p) => p,
                        None => return Ok(()),
                    };
                    let buf_ptr = self
                        .builder
                        .build_struct_gep(agg_ty, vec_ptr, 0, "vec_slc_buf_ptr")
                        .unwrap();
                    self.builder
                        .build_load(
                            self.ctx.ptr_type(inkwell::AddressSpace::default()),
                            buf_ptr,
                            "vec_slc_buf",
                        )
                        .unwrap()
                        .into_pointer_value()
                } else {
                    match self.build_place_gep_chain(&place, elem_ty) {
                        Some(p) => p,
                        None => return Ok(()),
                    }
                };
                let elem_llvm_ty = gruel_type_to_llvm(elem_ty, self.ctx, self.type_pool);
                let data_ptr = match elem_llvm_ty {
                    Some(ety) => unsafe {
                        self.builder
                            .build_gep(ety, arr_ptr, &[lo_val], "slc_ptr")
                            .unwrap()
                    },
                    None => arr_ptr, // zero-sized element — pointer arithmetic is a no-op
                };

                let len_val = self
                    .builder
                    .build_int_sub(hi_val, lo_val, "slc_len")
                    .unwrap();

                // Build the {ptr, i64} aggregate.
                let slice_llvm_ty = gruel_type_to_llvm(ty, self.ctx, self.type_pool)
                    .expect("slice type has LLVM lowering")
                    .into_struct_type();
                let undef = slice_llvm_ty.get_undef();
                let with_ptr = self
                    .builder
                    .build_insert_value(undef, data_ptr, 0, "slc_p")
                    .unwrap();
                let agg = self
                    .builder
                    .build_insert_value(with_ptr, len_val, 1, "slc")
                    .unwrap();
                Some(agg.into_struct_value().into())
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
                let ptr_val = self
                    .fn_value
                    .get_nth_param(llvm_idx)
                    .expect("param_slot out of range")
                    .into_pointer_value();
                self.builder.build_store(ptr_val, v).unwrap();
                None
            }

            // ---- Function calls ----
            CfgInstData::Call {
                name,
                args_start,
                args_len,
            } => {
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
                                CfgInstData::Param { index } if self.cfg.is_param_by_ref(index) => {
                                    // Forwarding an inout/borrow param: pass the raw pointer.
                                    let llvm_idx = self.slot_to_llvm_param[index as usize];
                                    let ptr = self
                                        .fn_value
                                        .get_nth_param(llvm_idx)
                                        .expect("param slot out of range");
                                    Some(inkwell::values::BasicMetadataValueEnum::from(ptr))
                                }
                                _ => {
                                    // Source has no stable storage address
                                    // (by-value param, computed value, etc.).
                                    // For most types, materialize into a
                                    // temporary alloca so we have something
                                    // to point at. Interface fat-pointers
                                    // (ADR-0056) are passed by value at the
                                    // ABI level even when the source-level
                                    // mode is borrow/inout — see
                                    // `is_param_by_ref`'s comment.
                                    let arg_ty = self.cfg.get_inst(arg.value).ty;
                                    if matches!(arg_ty.kind(), TypeKind::Interface(_)) {
                                        Some(self.get_value(arg.value).into())
                                    } else {
                                        let val = self.get_value(arg.value);
                                        let val_ty = val.get_type();
                                        let tmp = self.build_entry_alloca(val_ty, "borrow_tmp");
                                        self.builder.build_store(tmp, val).unwrap();
                                        Some(inkwell::values::BasicMetadataValueEnum::from(tmp))
                                    }
                                }
                            };
                        }
                        // Skip unit-typed (void) args — they have no LLVM representation.
                        let arg_ty = self.cfg.get_inst(arg.value).ty;
                        gruel_type_to_llvm(arg_ty, self.ctx, self.type_pool)?;
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
                let ret_is_struct =
                    matches!(ret_llvm, Some(inkwell::types::BasicTypeEnum::StructType(_)));

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

                    self.builder
                        .build_call(callee, &sret_call_args, "")
                        .unwrap();

                    // Load the result struct from the sret alloca.
                    let loaded = self
                        .builder
                        .build_load(struct_ty, sret_ptr, "sret_load")
                        .unwrap();
                    Some(loaded)
                } else {
                    // Normal call: look up in the declared-functions map, then fall back to the
                    // module. If not found anywhere, auto-declare as an external function.
                    let callee = self
                        .fn_map
                        .get(fn_name)
                        .copied()
                        .or_else(|| self.module.get_function(fn_name))
                        .unwrap_or_else(|| {
                            // Infer LLVM param types from the Gruel arg types.
                            let param_types: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> =
                                args.iter()
                                    .filter_map(|arg| {
                                        if arg.is_inout() || arg.is_borrow() {
                                            Some(
                                                self.ctx
                                                    .ptr_type(inkwell::AddressSpace::default())
                                                    .into(),
                                            )
                                        } else {
                                            let arg_ty = self.cfg.get_inst(arg.value).ty;
                                            gruel_type_to_llvm_param(
                                                arg_ty,
                                                self.ctx,
                                                self.type_pool,
                                            )
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

            CfgInstData::Intrinsic {
                name,
                args_start,
                args_len,
            } => {
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
                        self.builder
                            .build_int_s_extend(truncated, src_ty, "sext_chk")
                            .unwrap()
                    } else {
                        self.builder
                            .build_int_z_extend(truncated, src_ty, "zext_chk")
                            .unwrap()
                    };
                    // If extended != original, the value doesn't fit.
                    let fits = self
                        .builder
                        .build_int_compare(IntPredicate::EQ, v, extended, "fits")
                        .unwrap();
                    let fits = self.build_expect_i1(fits, true);
                    // Emit conditional branch to intcast overflow handler.
                    let current_fn = self
                        .builder
                        .get_insert_block()
                        .unwrap()
                        .get_parent()
                        .unwrap();
                    let overflow_bb = self.ctx.append_basic_block(current_fn, "icast_ovf");
                    let cont_bb = self.ctx.append_basic_block(current_fn, "icast_cont");
                    self.builder
                        .build_conditional_branch(fits, cont_bb, overflow_bb)
                        .unwrap();
                    // Overflow handler.
                    self.builder.position_at_end(overflow_bb);
                    let panic_fn = self.get_or_declare_noreturn_fn("__gruel_intcast_overflow");
                    self.builder.build_call(panic_fn, &[], "").unwrap();
                    self.builder.build_unreachable().unwrap();
                    // Continue.
                    self.builder.position_at_end(cont_bb);
                    truncated
                } else {
                    // Same-width cast between signed and unsigned types (or
                    // same-sign aliases like u64 → usize on 64-bit targets).
                    let src_signed = is_signed_type(from_ty);
                    let dst_signed = is_signed_type(ty);
                    if src_signed == dst_signed {
                        // Same sign, same width — no conversion needed (e.g. u64 ↔ usize).
                        v
                    } else {
                        // Check that the value is representable in the destination type.
                        let fits = if src_signed && !dst_signed {
                            // Signed → Unsigned: overflow if value < 0.
                            let zero = v.get_type().const_zero();
                            self.builder
                                .build_int_compare(IntPredicate::SGE, v, zero, "ick_fits")
                                .unwrap()
                        } else {
                            // Unsigned → Signed: overflow if value > INT_MAX.
                            let int_max_val = (i64::MAX as u64) >> (64u32.saturating_sub(src_bits));
                            let max = v.get_type().const_int(int_max_val, false);
                            self.builder
                                .build_int_compare(IntPredicate::ULE, v, max, "ick_fits")
                                .unwrap()
                        };
                        // Branch to overflow handler if the value is out of range.
                        let fits = self.build_expect_i1(fits, true);
                        let current_fn = self
                            .builder
                            .get_insert_block()
                            .unwrap()
                            .get_parent()
                            .unwrap();
                        let overflow_bb = self.ctx.append_basic_block(current_fn, "icast_ovf");
                        let cont_bb = self.ctx.append_basic_block(current_fn, "icast_cont");
                        self.builder
                            .build_conditional_branch(fits, cont_bb, overflow_bb)
                            .unwrap();
                        self.builder.position_at_end(overflow_bb);
                        let panic_fn = self.get_or_declare_noreturn_fn("__gruel_intcast_overflow");
                        self.builder.build_call(panic_fn, &[], "").unwrap();
                        self.builder.build_unreachable().unwrap();
                        self.builder.position_at_end(cont_bb);
                        v // Return original bits (reinterpreted as destination type)
                    }
                };
                Some(result.into())
            }

            // ---- Float cast (fptrunc / fpext) ----
            CfgInstData::FloatCast { value, from_ty: _ } => {
                let v = self.get_value(value).into_float_value();
                let dst_ty = gruel_type_to_llvm(ty, self.ctx, self.type_pool)
                    .expect("FloatCast target must be non-void")
                    .into_float_type();
                let src_bits = float_bit_width(v.get_type(), self.ctx);
                let dst_bits = float_bit_width(dst_ty, self.ctx);
                let result = if dst_bits < src_bits {
                    // Narrowing (e.g. f64 → f32): fptrunc
                    self.builder
                        .build_float_trunc(v, dst_ty, "fptrunc")
                        .unwrap()
                } else {
                    // Widening (e.g. f32 → f64): fpext
                    self.builder.build_float_ext(v, dst_ty, "fpext").unwrap()
                };
                Some(result.into())
            }

            // ---- Integer to float (sitofp / uitofp) ----
            CfgInstData::IntToFloat { value, from_ty } => {
                let v = self.get_value(value).into_int_value();
                let dst_ty = gruel_type_to_llvm(ty, self.ctx, self.type_pool)
                    .expect("IntToFloat target must be non-void")
                    .into_float_type();
                let result = if is_signed_type(from_ty) {
                    self.builder
                        .build_signed_int_to_float(v, dst_ty, "sitofp")
                        .unwrap()
                } else {
                    self.builder
                        .build_unsigned_int_to_float(v, dst_ty, "uitofp")
                        .unwrap()
                };
                Some(result.into())
            }

            // ---- Float to integer (fptosi / fptoui) with NaN + overflow check ----
            CfgInstData::FloatToInt { value, from_ty: _ } => {
                let v = self.get_value(value).into_float_value();
                let dst_ty = gruel_type_to_llvm(ty, self.ctx, self.type_pool)
                    .expect("FloatToInt target must be non-void")
                    .into_int_type();
                let dst_signed = is_signed_type(ty);
                let dst_bits = dst_ty.get_bit_width();

                // Check 1: NaN check — fcmp ord (ordered) returns false if either is NaN.
                let is_ordered = self
                    .builder
                    .build_float_compare(inkwell::FloatPredicate::ORD, v, v, "nan_chk")
                    .unwrap();

                // Check 2: Range check — value must be within [min, max] for the target integer.
                // For signed N-bit: min = -(2^(N-1)), max = 2^(N-1) - 1
                // For unsigned N-bit: min = 0, max = 2^N - 1
                // We use float comparisons: value >= min_float && value <= max_float
                // where min_float/max_float are the exact boundaries (or the nearest
                // representable float that doesn't overflow the integer).
                let float_ty = v.get_type();
                let (min_val, max_val) = if dst_signed {
                    let min = -(2.0_f64.powi(dst_bits as i32 - 1));
                    let max = 2.0_f64.powi(dst_bits as i32 - 1) - 1.0;
                    (min, max)
                } else {
                    let min = 0.0_f64;
                    // For u64, 2^64-1 is not exactly representable in f64.
                    // Use 2^64 - any float >= 2^64 overflows u64.
                    // We check value < 2^N (strict less) for unsigned.
                    let max = 2.0_f64.powi(dst_bits as i32);
                    (min, max)
                };

                let ge_min = if dst_signed {
                    // value >= min (where min is negative, e.g. -2^63)
                    let min_const = float_ty.const_float(min_val);
                    self.builder
                        .build_float_compare(inkwell::FloatPredicate::OGE, v, min_const, "ge_min")
                        .unwrap()
                } else {
                    // value >= 0.0
                    let zero = float_ty.const_float(0.0);
                    self.builder
                        .build_float_compare(inkwell::FloatPredicate::OGE, v, zero, "ge_min")
                        .unwrap()
                };

                let le_max = if dst_signed {
                    // value <= max
                    let max_const = float_ty.const_float(max_val);
                    self.builder
                        .build_float_compare(inkwell::FloatPredicate::OLE, v, max_const, "le_max")
                        .unwrap()
                } else {
                    // value < 2^N (strict less, since 2^N itself overflows)
                    let max_const = float_ty.const_float(max_val);
                    self.builder
                        .build_float_compare(inkwell::FloatPredicate::OLT, v, max_const, "lt_max")
                        .unwrap()
                };

                // Combine: not_nan && in_range
                let in_range = self.builder.build_and(ge_min, le_max, "in_range").unwrap();
                let valid = self
                    .builder
                    .build_and(is_ordered, in_range, "f2i_valid")
                    .unwrap();
                let valid = self.build_expect_i1(valid, true);

                let current_fn = self
                    .builder
                    .get_insert_block()
                    .unwrap()
                    .get_parent()
                    .unwrap();
                let overflow_bb = self.ctx.append_basic_block(current_fn, "f2i_ovf");
                let cont_bb = self.ctx.append_basic_block(current_fn, "f2i_cont");
                self.builder
                    .build_conditional_branch(valid, cont_bb, overflow_bb)
                    .unwrap();

                // Overflow/NaN handler
                self.builder.position_at_end(overflow_bb);
                let panic_fn = self.get_or_declare_noreturn_fn("__gruel_float_to_int_overflow");
                self.builder.build_call(panic_fn, &[], "").unwrap();
                self.builder.build_unreachable().unwrap();

                // Continue with the conversion
                self.builder.position_at_end(cont_bb);
                let result = if dst_signed {
                    self.builder
                        .build_float_to_signed_int(v, dst_ty, "fptosi")
                        .unwrap()
                } else {
                    self.builder
                        .build_float_to_unsigned_int(v, dst_ty, "fptoui")
                        .unwrap()
                };
                Some(result.into())
            }

            // ---- Drop / storage liveness ----
            CfgInstData::Drop {
                value: dropped_value,
            } => {
                let dropped_ty = self.cfg.get_inst(dropped_value).ty;
                // ADR-0066: Vec(T) drop. For T:Copy v1, just free the buffer
                // if cap > 0. (Per-element drops for non-Copy T land in
                // Phase 6.)
                if matches!(dropped_ty.kind(), TypeKind::Vec(_))
                    && let Some(val) = self.values[dropped_value.as_u32() as usize]
                {
                    self.translate_vec_drop(val, dropped_ty);
                    return Ok(());
                }
                if self.is_builtin_string(dropped_ty) {
                    // String: call __gruel_drop_String(ptr, len, cap) from the runtime.
                    // Literals have cap == 0 and are safely treated as no-ops.
                    if let Some(str_val) = self.values[dropped_value.as_u32() as usize] {
                        let (ptr, len, cap) = self.extract_str_ptr_len_cap(str_val);
                        let drop_fn = self.get_or_declare_drop_string();
                        self.builder
                            .build_call(drop_fn, &[ptr.into(), len.into(), cap.into()], "")
                            .unwrap();
                    }
                } else if let Some(fn_name) = drop_names::drop_fn_name(dropped_ty, self.type_pool) {
                    // Non-trivial struct or array: call the synthesized __gruel_drop_* function.
                    //
                    // Struct drop glue takes the whole struct as a single LLVM parameter,
                    // so we pass the value directly.
                    //
                    // Array drop glue takes each element as a separate LLVM parameter,
                    // so we extract and pass them individually.
                    if let Some(val) = self.values[dropped_value.as_u32() as usize] {
                        let args: Vec<BasicValueEnum<'ctx>> = match dropped_ty.kind() {
                            TypeKind::Array(_) => self.extract_fields_for_drop(val, dropped_ty),
                            _ => vec![val],
                        };
                        let param_types: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> =
                            args.iter().map(|v| v.get_type().into()).collect();
                        let meta_args: Vec<BasicMetadataValueEnum<'ctx>> =
                            args.iter().map(|v| (*v).into()).collect();
                        let fn_in_map = self.fn_map.get(fn_name.as_str()).copied();
                        let fn_in_module = self.module.get_function(&fn_name);
                        let callee = fn_in_map.or(fn_in_module).unwrap_or_else(|| {
                            let fn_ty = self.ctx.void_type().fn_type(&param_types, false);
                            self.module.add_function(&fn_name, fn_ty, None)
                        });
                        self.builder.build_call(callee, &meta_args, "").unwrap();
                    }
                }
                None
            }
            CfgInstData::StorageLive { .. } | CfgInstData::StorageDead { .. } => None,

            // ---- Composite ops (Phase 2d) ----
            CfgInstData::EnumVariant {
                enum_id,
                variant_index,
            } => {
                let enum_def = self.type_pool.enum_def(enum_id);
                if enum_def.is_unit_only() {
                    // Unit-only enum: represented as its discriminant integer.
                    if let Some(t) = gruel_type_to_llvm(ty, self.ctx, self.type_pool) {
                        let v = t
                            .into_int_type()
                            .const_int(variant_index as u64, false)
                            .into();
                        self.set_value(val, v);
                    }
                    return Ok(());
                }
                let layout = layout_of(self.type_pool, ty);
                let strategy = layout
                    .discriminant_strategy()
                    .expect("data enum must have a discriminant strategy");
                if let DiscriminantStrategy::Niche {
                    unit_variant,
                    niche_offset,
                    niche_width,
                    niche_value,
                    ..
                } = strategy
                {
                    // Niche-encoded layout: storage is `[size x i8]`.
                    debug_assert_eq!(variant_index, unit_variant);
                    let storage_ty = self.ctx.i8_type().array_type(layout.size as u32);
                    let slot = self.build_entry_alloca(storage_ty.into(), "niche_unit_slot");
                    let zero_init = storage_ty.const_zero();
                    self.builder.build_store(slot, zero_init).unwrap();
                    self.write_niche_bytes(
                        slot,
                        storage_ty,
                        niche_offset,
                        niche_width,
                        niche_value,
                    );
                    let result = self
                        .builder
                        .build_load(storage_ty, slot, "niche_unit_val")
                        .unwrap();
                    self.set_value(val, result);
                    return Ok(());
                }
                // Data enum (Separate strategy): unit variant produces a tagged
                // union value with the discriminant set and the payload zeroed.
                let struct_ty = match gruel_type_to_llvm(ty, self.ctx, self.type_pool) {
                    Some(t) => t.into_struct_type(),
                    None => return Ok(()),
                };
                let discrim_ty =
                    gruel_type_to_llvm(enum_def.discriminant_type(), self.ctx, self.type_pool)
                        .unwrap()
                        .into_int_type();
                let discrim_val = discrim_ty.const_int(variant_index as u64, false);
                let mut agg: AggregateValueEnum = struct_ty.get_undef().into();
                agg = self
                    .builder
                    .build_insert_value(agg, discrim_val, 0, "ev_d")
                    .expect("build_insert_value failed");
                // Payload (field 1) is zeroed for unit variants.
                let payload_ty = struct_ty.get_field_type_at_index(1).unwrap();
                let payload_zero = payload_ty.const_zero();
                agg = self
                    .builder
                    .build_insert_value(agg, payload_zero, 1, "ev_p")
                    .expect("build_insert_value failed");
                Some(agg.as_basic_value_enum())
            }

            CfgInstData::EnumCreate {
                enum_id,
                variant_index,
                fields_start,
                fields_len,
            } => {
                let enum_def = self.type_pool.enum_def(enum_id);
                let variant_def = &enum_def.variants[variant_index as usize];
                let field_types: Vec<Type> = variant_def.fields.clone();

                let layout = layout_of(self.type_pool, ty);
                let strategy = layout
                    .discriminant_strategy()
                    .expect("enum type must have a discriminant strategy");

                match strategy {
                    DiscriminantStrategy::Niche {
                        unit_variant,
                        niche_offset,
                        niche_width,
                        niche_value,
                        ..
                    } => {
                        // ADR-0069: niche-encoded layout is `[size x i8]`. The
                        // unit variant writes `niche_value` at `niche_offset`;
                        // the data variant stores its single payload field
                        // starting at offset 0.
                        let storage_ty = self.ctx.i8_type().array_type(layout.size as u32);
                        let slot = self.build_entry_alloca(storage_ty.into(), "enum_slot");
                        // Zero the storage so unused bytes are deterministic.
                        let zero_init = storage_ty.const_zero();
                        self.builder.build_store(slot, zero_init).unwrap();

                        let fields = self.cfg.get_extra(fields_start, fields_len).to_vec();
                        if variant_index == unit_variant {
                            // Unit variant: write niche_value at niche_offset.
                            self.write_niche_bytes(
                                slot,
                                storage_ty,
                                niche_offset,
                                niche_width,
                                niche_value,
                            );
                        } else {
                            // Data variant: store the (single) payload field at offset 0.
                            debug_assert_eq!(fields.len(), 1);
                            debug_assert_eq!(field_types.len(), 1);
                            let field_ty = field_types[0];
                            if gruel_type_to_llvm(field_ty, self.ctx, self.type_pool).is_some() {
                                let field_llvm_val = self.get_value(fields[0]);
                                let zero = self.ctx.i64_type().const_zero();
                                let payload_ptr = unsafe {
                                    self.builder
                                        .build_gep(
                                            storage_ty,
                                            slot,
                                            &[zero, zero],
                                            "niche_payload_ptr",
                                        )
                                        .expect("build_gep failed")
                                };
                                let store = self
                                    .builder
                                    .build_store(payload_ptr, field_llvm_val)
                                    .unwrap();
                                store.set_alignment(1).unwrap();
                            }
                        }

                        let result = self
                            .builder
                            .build_load(storage_ty, slot, "enum_val")
                            .unwrap();
                        self.set_value(val, result);
                        return Ok(());
                    }
                    DiscriminantStrategy::Separate { .. } => {}
                }

                let struct_ty = match gruel_type_to_llvm(ty, self.ctx, self.type_pool) {
                    Some(t) => t.into_struct_type(),
                    None => return Ok(()),
                };

                // Alloca the tagged union on the stack (in entry block for mem2reg).
                let slot = self.build_entry_alloca(struct_ty.into(), "enum_slot");

                // Store discriminant at struct field 0 (Separate strategy: tag_offset = 0).
                let discrim_ty =
                    gruel_type_to_llvm(enum_def.discriminant_type(), self.ctx, self.type_pool)
                        .unwrap()
                        .into_int_type();
                let discrim_val = discrim_ty.const_int(variant_index as u64, false);
                let discrim_ptr = self
                    .builder
                    .build_struct_gep(struct_ty, slot, 0, "discrim_ptr")
                    .expect("build_struct_gep failed");
                self.builder.build_store(discrim_ptr, discrim_val).unwrap();

                // Store each field value into the payload (struct field 1) at its byte offset.
                let fields = self.cfg.get_extra(fields_start, fields_len).to_vec();
                if !fields.is_empty() {
                    let variant_payload_size: u64 = field_types
                        .iter()
                        .map(|f| layout_of(self.type_pool, *f).size)
                        .sum();
                    let byte_arr_ty = self.ctx.i8_type().array_type(variant_payload_size as u32);
                    let payload_ptr = self
                        .builder
                        .build_struct_gep(struct_ty, slot, 1, "payload_ptr")
                        .expect("build_struct_gep failed");

                    let mut byte_offset = 0u64;
                    for (i, field_val) in fields.iter().enumerate() {
                        let field_ty = field_types[i];
                        if gruel_type_to_llvm(field_ty, self.ctx, self.type_pool).is_none() {
                            // Zero-sized type — skip (no bytes to store).
                            continue;
                        }
                        let field_llvm_val = self.get_value(*field_val);
                        let offset_const = self.ctx.i64_type().const_int(byte_offset, false);
                        let zero = self.ctx.i64_type().const_zero();
                        // GEP into the byte array to find the write pointer.
                        let field_ptr = unsafe {
                            self.builder
                                .build_gep(
                                    byte_arr_ty,
                                    payload_ptr,
                                    &[zero, offset_const],
                                    "field_ptr",
                                )
                                .expect("build_gep failed")
                        };
                        // Use alignment 1 (unaligned store): the byte array payload may not
                        // satisfy the natural alignment of the field type.
                        let store = self.builder.build_store(field_ptr, field_llvm_val).unwrap();
                        store.set_alignment(1).unwrap();

                        byte_offset += layout_of(self.type_pool, field_ty).size;
                    }
                }

                // Load and return the completed tagged union value.
                let result = self
                    .builder
                    .build_load(struct_ty, slot, "enum_val")
                    .unwrap();
                Some(result)
            }

            CfgInstData::EnumPayloadGet {
                base,
                variant_index,
                field_index,
            } => {
                // Extract field `field_index` from variant `variant_index`'s payload.
                // The base value is { discriminant, [N x i8] }.
                // We alloca it, GEP to the payload byte array, GEP to the field offset, load.
                let base_val = self.get_value(base);
                let scrutinee_inst = self.cfg.get_inst(base);
                let enum_ty = scrutinee_inst.ty;
                let TypeKind::Enum(enum_id) = enum_ty.kind() else {
                    panic!("EnumPayloadGet: base is not an enum type");
                };
                let enum_def = self.type_pool.enum_def(enum_id);
                let variant_def = &enum_def.variants[variant_index as usize];
                let field_types = &variant_def.fields;

                let layout = layout_of(self.type_pool, enum_ty);
                let strategy = layout
                    .discriminant_strategy()
                    .expect("enum type must have a discriminant strategy");
                if let DiscriminantStrategy::Niche { .. } = strategy {
                    // Niche-encoded enum: storage is `[size x i8]`, payload
                    // occupies offset 0, and there is exactly one field.
                    debug_assert_eq!(field_index, 0);
                    debug_assert_eq!(field_types.len(), 1);
                    let field_ty_gruel = field_types[0];
                    let Some(field_llvm_ty) =
                        gruel_type_to_llvm(field_ty_gruel, self.ctx, self.type_pool)
                    else {
                        return Ok(());
                    };
                    let storage_ty = self.ctx.i8_type().array_type(layout.size as u32);
                    let slot = self.build_entry_alloca(storage_ty.into(), "niche_payload_slot");
                    self.builder.build_store(slot, base_val).unwrap();
                    let zero = self.ctx.i64_type().const_zero();
                    let field_ptr = unsafe {
                        self.builder
                            .build_gep(storage_ty, slot, &[zero, zero], "niche_payload_ptr")
                            .expect("build_gep failed")
                    };
                    let load = self
                        .builder
                        .build_load(field_llvm_ty, field_ptr, "niche_payload_val")
                        .unwrap();
                    load.as_instruction_value()
                        .unwrap()
                        .set_alignment(1)
                        .unwrap();
                    self.set_value(val, load);
                    return Ok(());
                }

                // Compute byte offset of the target field within the payload.
                let byte_offset: u64 = field_types[..field_index as usize]
                    .iter()
                    .map(|f| layout_of(self.type_pool, *f).size)
                    .sum();

                let field_ty_gruel = field_types[field_index as usize];
                let field_llvm_ty =
                    match gruel_type_to_llvm(field_ty_gruel, self.ctx, self.type_pool) {
                        Some(t) => t,
                        None => return Ok(()), // zero-sized field
                    };

                // Get the LLVM struct type for the enum tagged union.
                let enum_llvm_ty = gruel_type_to_llvm(enum_ty, self.ctx, self.type_pool)
                    .expect("data enum must have LLVM type")
                    .into_struct_type();

                // Alloca the base value to get a pointer.
                let slot = self.build_entry_alloca(enum_llvm_ty.into(), "enum_payload_slot");
                self.builder.build_store(slot, base_val).unwrap();

                // GEP to field 1 of the struct (the payload byte array).
                let payload_ptr = self
                    .builder
                    .build_struct_gep(enum_llvm_ty, slot, 1, "payload_ptr")
                    .expect("build_struct_gep failed");

                // GEP into the payload byte array at the field's byte offset.
                let max_payload: u64 = enum_def
                    .variants
                    .iter()
                    .map(|v| {
                        v.fields
                            .iter()
                            .map(|f| layout_of(self.type_pool, *f).size)
                            .sum::<u64>()
                    })
                    .max()
                    .unwrap_or(0);
                let byte_arr_ty = self.ctx.i8_type().array_type(max_payload as u32);
                let zero = self.ctx.i64_type().const_zero();
                let offset_const = self.ctx.i64_type().const_int(byte_offset, false);
                let field_ptr = unsafe {
                    self.builder
                        .build_gep(byte_arr_ty, payload_ptr, &[zero, offset_const], "field_ptr")
                        .expect("build_gep failed")
                };

                // Load the field value with alignment 1 (unaligned; payload is a byte array).
                let load = self
                    .builder
                    .build_load(field_llvm_ty, field_ptr, "field_val")
                    .unwrap();
                load.as_instruction_value()
                    .unwrap()
                    .set_alignment(1)
                    .unwrap();
                Some(load)
            }

            CfgInstData::GetDiscriminant { base } => {
                // Read the discriminant according to the type's
                // `DiscriminantStrategy` (ADR-0069). For the current
                // `Separate` strategy with data variants the LLVM layout is
                // `{ disc, payload }`, so we extract element 0; for unit-only
                // enums the value itself is the discriminant.
                // Mirrors the discriminant-read logic in
                // `Terminator::Switch` below.
                let raw = self.get_value(base);
                let scrutinee_ty = self.cfg.get_inst(base).ty;
                let disc = if let TypeKind::Enum(id) = scrutinee_ty.kind() {
                    let enum_def = self.type_pool.enum_def(id);
                    let layout = layout_of(self.type_pool, scrutinee_ty);
                    let strategy = layout
                        .discriminant_strategy()
                        .expect("enum type must have a discriminant strategy");
                    match strategy {
                        DiscriminantStrategy::Separate { .. } => {
                            if enum_def.has_data_variants() {
                                let struct_val = raw.into_struct_value();
                                self.builder
                                    .build_extract_value(struct_val, 0, "discrim")
                                    .expect("extract_value failed")
                            } else {
                                raw
                            }
                        }
                        DiscriminantStrategy::Niche {
                            unit_variant,
                            data_variant,
                            niche_offset,
                            niche_width,
                            niche_value,
                        } => self
                            .synthesize_niche_discriminant(
                                raw,
                                layout.size,
                                niche_offset,
                                niche_width,
                                niche_value,
                                unit_variant,
                                data_variant,
                                enum_def.discriminant_type(),
                            )
                            .into(),
                    }
                } else {
                    raw
                };
                Some(disc)
            }

            CfgInstData::StructInit {
                struct_id,
                fields_start,
                fields_len,
            } => {
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
                        agg = self
                            .builder
                            .build_insert_value(agg, fv, llvm_idx, "si")
                            .expect("build_insert_value failed");
                        llvm_idx += 1;
                    }
                }
                Some(agg.as_basic_value_enum())
            }

            CfgInstData::ArrayInit {
                elements_start,
                elements_len,
            } => {
                let elements = self.cfg.get_extra(elements_start, elements_len).to_vec();
                let arr_llvm_ty = match gruel_type_to_llvm(ty, self.ctx, self.type_pool) {
                    Some(t) => t.into_array_type(),
                    None => return Ok(()), // void array — no representation
                };
                let mut agg: AggregateValueEnum = arr_llvm_ty.get_undef().into();
                for (i, &elem_val) in elements.iter().enumerate() {
                    let v = self.get_value(elem_val);
                    agg = self
                        .builder
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

            CfgInstData::FieldSet {
                slot,
                struct_id,
                field_index,
                value: val,
            } => {
                let val_ty = self.cfg.get_inst(val).ty;
                if gruel_type_to_llvm(val_ty, self.ctx, self.type_pool).is_some() {
                    let v = self.get_value(val);
                    let struct_ty = Type::new_struct(struct_id);
                    let ptr = self.get_or_create_local(slot, struct_ty);
                    let llvm_field_idx = self.gruel_to_llvm_field_index(struct_id, field_index);
                    let struct_llvm_ty = gruel_type_to_llvm(struct_ty, self.ctx, self.type_pool)
                        .expect("struct must have LLVM type")
                        .into_struct_type();
                    let field_ptr = self
                        .builder
                        .build_struct_gep(struct_llvm_ty, ptr, llvm_field_idx, "fsgep")
                        .expect("build_struct_gep failed");
                    self.builder.build_store(field_ptr, v).unwrap();
                }
                None
            }

            CfgInstData::IndexSet {
                slot,
                array_type,
                index,
                value: val,
            } => {
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
            CfgInstData::ParamFieldSet {
                param_slot,
                struct_id,
                field_index,
                value: val,
                ..
            } => {
                let val_ty = self.cfg.get_inst(val).ty;
                if gruel_type_to_llvm(val_ty, self.ctx, self.type_pool).is_some() {
                    let v = self.get_value(val);
                    let struct_ty = Type::new_struct(struct_id);
                    let llvm_idx = self.slot_to_llvm_param[param_slot as usize];
                    let base_ptr = if self.cfg.is_param_by_ref(param_slot) {
                        self.fn_value
                            .get_nth_param(llvm_idx)
                            .expect("param slot out of range")
                            .into_pointer_value()
                    } else {
                        self.get_or_create_param_alloca(param_slot, struct_ty)
                    };
                    let llvm_field_idx = self.gruel_to_llvm_field_index(struct_id, field_index);
                    let struct_llvm_ty = gruel_type_to_llvm(struct_ty, self.ctx, self.type_pool)
                        .expect("struct must have LLVM type")
                        .into_struct_type();
                    let field_ptr = self
                        .builder
                        .build_struct_gep(struct_llvm_ty, base_ptr, llvm_field_idx, "pfsgep")
                        .expect("build_struct_gep failed");
                    self.builder.build_store(field_ptr, v).unwrap();
                }
                None
            }

            CfgInstData::ParamIndexSet {
                param_slot,
                array_type,
                index,
                value: val,
            } => {
                let val_ty = self.cfg.get_inst(val).ty;
                if gruel_type_to_llvm(val_ty, self.ctx, self.type_pool).is_some() {
                    let v = self.get_value(val);
                    let arr_id = array_type.as_array().expect("ParamIndexSet on non-array");
                    let (_elem_ty, length) = self.type_pool.array_def(arr_id);
                    let index_val = self.get_value(index).into_int_value();
                    self.build_bounds_check(index_val, length);
                    let llvm_idx2 = self.slot_to_llvm_param[param_slot as usize];
                    let base_ptr = if self.cfg.is_param_by_ref(param_slot) {
                        self.fn_value
                            .get_nth_param(llvm_idx2)
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

            CfgInstData::MakeInterfaceRef {
                value,
                struct_id,
                interface_id,
            } => {
                // ADR-0056 Phase 4d: build a fat pointer `(data_ptr, vtable_ptr)`.
                //
                // The data pointer addresses the source value:
                //   - Load { slot }  → use the local's alloca directly.
                //   - Param { index } if by-ref → use the incoming ptr param.
                //   - Param { index } else → spill the param value to an
                //     alloca and use that.
                //   - Otherwise → materialize a fresh alloca, store the
                //     value into it, and use that.
                let source_inst = self.cfg.get_inst(value).clone();
                let source_ty = source_inst.ty;
                // Zero-sized source types (empty structs) have no LLVM type
                // and no storage. The data pointer is never dereferenced for
                // such types, so a null pointer is a valid placeholder.
                let source_llvm_ty = gruel_type_to_llvm(source_ty, self.ctx, self.type_pool);
                let data_ptr: inkwell::values::PointerValue<'ctx> = match source_llvm_ty {
                    None => self
                        .ctx
                        .ptr_type(inkwell::AddressSpace::default())
                        .const_null(),
                    Some(llvm_ty) => match source_inst.data {
                        CfgInstData::Load { slot } => self
                            .locals
                            .get(slot as usize)
                            .copied()
                            .flatten()
                            .unwrap_or_else(|| {
                                self.ctx
                                    .ptr_type(inkwell::AddressSpace::default())
                                    .const_null()
                            }),
                        CfgInstData::Param { index } => {
                            if self.cfg.is_param_by_ref(index) {
                                let llvm_idx = self.slot_to_llvm_param[index as usize];
                                self.fn_value
                                    .get_nth_param(llvm_idx)
                                    .expect("param out of range")
                                    .into_pointer_value()
                            } else {
                                self.get_or_create_param_alloca(index, source_ty)
                            }
                        }
                        _ => {
                            // Materialize an alloca and store the value into
                            // it.
                            let ptr = self.build_entry_alloca(llvm_ty, "iface.tmp");
                            let v = self.get_value(value);
                            self.builder.build_store(ptr, v).unwrap();
                            ptr
                        }
                    },
                };

                // ADR-0056 Phase 4d-extended: the vtable pointer is the
                // address of a per-(struct, interface) global emitted by
                // build_module. Falls back to null if (somehow) the pair
                // isn't in the map, which shouldn't happen in well-formed
                // sema output but keeps codegen robust.
                let vtable_ptr = self
                    .vtable_map
                    .get(&(struct_id, interface_id))
                    .map(|g| g.as_pointer_value())
                    .unwrap_or_else(|| {
                        self.ctx
                            .ptr_type(inkwell::AddressSpace::default())
                            .const_null()
                    });

                let iface_struct_ty = gruel_type_to_llvm(ty, self.ctx, self.type_pool)
                    .expect("interface type must have LLVM representation")
                    .into_struct_type();
                let undef = iface_struct_ty.get_undef();
                let with_data = self
                    .builder
                    .build_insert_value(undef, data_ptr, 0, "iface.data")
                    .unwrap();
                let with_vtbl = self
                    .builder
                    .build_insert_value(with_data, vtable_ptr, 1, "iface.vt")
                    .unwrap();
                Some(with_vtbl.into_struct_value().into())
            }

            CfgInstData::MethodCallDyn {
                interface_id,
                slot,
                recv,
                args_start,
                args_len,
            } => {
                // ADR-0056 Phase 4d-extended: dynamic dispatch through the
                // fat pointer's vtable. Steps:
                //   1. Extract data_ptr (field 0) and vtable_ptr (field 1)
                //      from the receiver fat pointer struct.
                //   2. GEP into vtable[slot] to get the function pointer
                //      address; load the pointer.
                //   3. Construct an indirect-call function type from the
                //      interface's method signature: first arg is `ptr`
                //      (the data pointer), then the additional args.
                //   4. Call indirectly.
                //
                // For Phase 4d-ext MVP, the conforming type's method
                // signature is required to be the same shape as the
                // interface's signature (minus the `self` substitution).
                // We model `self` as a `ptr` at the LLVM ABI: the callee's
                // first LLVM parameter is the data pointer.
                let recv_val = self.get_value(recv).into_struct_value();
                let data_ptr = self
                    .builder
                    .build_extract_value(recv_val, 0, "iface.data")
                    .unwrap()
                    .into_pointer_value();
                let vtable_ptr = self
                    .builder
                    .build_extract_value(recv_val, 1, "iface.vt")
                    .unwrap()
                    .into_pointer_value();

                let iface_def = &self.interface_defs[interface_id.0 as usize];
                let req = &iface_def.methods[slot as usize];
                let n_methods = iface_def.methods.len().max(1) as u32;
                let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
                let array_ty = ptr_ty.array_type(n_methods);

                // GEP into the vtable: vtable[slot] is a `ptr`.
                let zero = self.ctx.i32_type().const_zero();
                let slot_idx = self.ctx.i32_type().const_int(slot as u64, false);
                let slot_ptr = unsafe {
                    self.builder
                        .build_gep(array_ty, vtable_ptr, &[zero, slot_idx], "vt.slot")
                        .expect("build_gep failed")
                };
                let fn_ptr = self
                    .builder
                    .build_load(ptr_ty, slot_ptr, "vt.fn")
                    .unwrap()
                    .into_pointer_value();

                // Build the function type. First arg is `ptr` (the data
                // pointer carrying the receiver). Subsequent args follow
                // the interface's declared parameter types. `Self` slots
                // (ADR-0060) substitute to the interface type itself at the
                // dynamic dispatch boundary.
                let iface_self_ty = gruel_air::Type::new_interface(interface_id);
                let mut llvm_param_types: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> =
                    Vec::with_capacity(req.param_types.len() + 1);
                llvm_param_types.push(ptr_ty.into());
                for pty in req.param_types.iter() {
                    let concrete = pty.substitute_self(iface_self_ty);
                    if let Some(ll) = gruel_type_to_llvm_param(concrete, self.ctx, self.type_pool) {
                        llvm_param_types.push(ll);
                    }
                }
                let ret_llvm = gruel_type_to_llvm(
                    req.return_type.substitute_self(iface_self_ty),
                    self.ctx,
                    self.type_pool,
                );
                let fn_type = match ret_llvm {
                    Some(t) => t.fn_type(&llvm_param_types, false),
                    None => self.ctx.void_type().fn_type(&llvm_param_types, false),
                };

                // Gather the additional args (already lowered into the
                // call_args extra array by the CFG builder).
                let cfg_args = self.cfg.get_call_args(args_start, args_len);
                let mut llvm_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> =
                    Vec::with_capacity(cfg_args.len() + 1);
                llvm_args.push(data_ptr.into());
                for arg in cfg_args {
                    let v = self.get_value(arg.value);
                    llvm_args.push(v.into());
                }

                let call_site = self
                    .builder
                    .build_indirect_call(fn_type, fn_ptr, &llvm_args, "iface.dispatch")
                    .unwrap();

                call_site.try_as_basic_value().basic()
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
    ///
    /// Dispatches on the stable `IntrinsicId` resolved from the
    /// `gruel-intrinsics` registry (ADR-0050). Runtime extern symbols come from
    /// the registry's `runtime_fn` field where applicable.
    fn translate_intrinsic(
        &mut self,
        ty: Type,
        name_str: &str,
        args: &[CfgValue],
    ) -> Option<BasicValueEnum<'ctx>> {
        let id = match lookup_by_name(name_str).map(|d| d.id) {
            Some(id) => id,
            None => {
                // Unknown name — same fall-through semantics as the legacy match.
                return gruel_type_to_llvm(ty, self.ctx, self.type_pool).map(|t| t.const_zero());
            }
        };
        match id {
            // ---- Random number generation ----
            IntrinsicId::RandomU32 => {
                let runtime_fn = lookup_by_name("random_u32")
                    .and_then(|d| d.runtime_fn)
                    .expect("random_u32 has a runtime symbol");
                let fn_ty = self.ctx.i32_type().fn_type(&[], false);
                let f = self
                    .module
                    .get_function(runtime_fn)
                    .unwrap_or_else(|| self.module.add_function(runtime_fn, fn_ty, None));
                self.builder
                    .build_call(f, &[], "rand")
                    .unwrap()
                    .try_as_basic_value()
                    .basic()
            }
            IntrinsicId::RandomU64 => {
                let runtime_fn = lookup_by_name("random_u64")
                    .and_then(|d| d.runtime_fn)
                    .expect("random_u64 has a runtime symbol");
                let fn_ty = self.ctx.i64_type().fn_type(&[], false);
                let f = self
                    .module
                    .get_function(runtime_fn)
                    .unwrap_or_else(|| self.module.add_function(runtime_fn, fn_ty, None));
                self.builder
                    .build_call(f, &[], "rand")
                    .unwrap()
                    .try_as_basic_value()
                    .basic()
            }

            // ---- User-triggered panic ----
            IntrinsicId::Panic => {
                let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.ctx.i64_type();
                if args.is_empty() {
                    // @panic() — no message. Calls __gruel_panic_no_msg().
                    let fn_ty = self.ctx.void_type().fn_type(&[], false);
                    let f = self
                        .module
                        .get_function("__gruel_panic_no_msg")
                        .unwrap_or_else(|| {
                            self.module
                                .add_function("__gruel_panic_no_msg", fn_ty, None)
                        });
                    self.builder.build_call(f, &[], "").unwrap();
                } else {
                    // @panic(msg: String) — extract (ptr, len) and call __gruel_panic.
                    let msg_val = self.get_value(args[0]);
                    let (ptr, len) = self.extract_str_ptr_len(msg_val);
                    let fn_ty = self
                        .ctx
                        .void_type()
                        .fn_type(&[ptr_ty.into(), i64_ty.into()], false);
                    let f = self
                        .module
                        .get_function("__gruel_panic")
                        .unwrap_or_else(|| self.module.add_function("__gruel_panic", fn_ty, None));
                    self.builder
                        .build_call(f, &[ptr.into(), len.into()], "")
                        .unwrap();
                }
                None
            }

            // ---- Debug print ----
            IntrinsicId::Dbg => {
                let i64_ty = self.ctx.i64_type();
                let void_noarg_ty = self.ctx.void_type().fn_type(&[], false);
                let space_fn = self
                    .module
                    .get_function("__gruel_dbg_space")
                    .unwrap_or_else(|| {
                        self.module
                            .add_function("__gruel_dbg_space", void_noarg_ty, None)
                    });
                let newline_fn = self
                    .module
                    .get_function("__gruel_dbg_newline")
                    .unwrap_or_else(|| {
                        self.module
                            .add_function("__gruel_dbg_newline", void_noarg_ty, None)
                    });
                for (i, arg_val) in args.iter().copied().enumerate() {
                    if i > 0 {
                        self.builder.build_call(space_fn, &[], "").unwrap();
                    }
                    let arg_ty = self.cfg.get_inst(arg_val).ty;
                    match arg_ty.kind() {
                        TypeKind::I8 | TypeKind::I16 | TypeKind::I32 | TypeKind::I64 => {
                            let v = self.get_value(arg_val).into_int_value();
                            let v64 = if v.get_type().get_bit_width() < 64 {
                                self.builder.build_int_s_extend(v, i64_ty, "sext").unwrap()
                            } else {
                                v
                            };
                            let fn_ty = self.ctx.void_type().fn_type(&[i64_ty.into()], false);
                            let f = self
                                .module
                                .get_function("__gruel_dbg_i64_noln")
                                .unwrap_or_else(|| {
                                    self.module
                                        .add_function("__gruel_dbg_i64_noln", fn_ty, None)
                                });
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
                            let f = self
                                .module
                                .get_function("__gruel_dbg_u64_noln")
                                .unwrap_or_else(|| {
                                    self.module
                                        .add_function("__gruel_dbg_u64_noln", fn_ty, None)
                                });
                            self.builder.build_call(f, &[v64.into()], "").unwrap();
                        }
                        TypeKind::Bool => {
                            let v = self.get_value(arg_val).into_int_value();
                            let v64 = self.builder.build_int_z_extend(v, i64_ty, "zext").unwrap();
                            let fn_ty = self.ctx.void_type().fn_type(&[i64_ty.into()], false);
                            let f = self
                                .module
                                .get_function("__gruel_dbg_bool_noln")
                                .unwrap_or_else(|| {
                                    self.module
                                        .add_function("__gruel_dbg_bool_noln", fn_ty, None)
                                });
                            self.builder.build_call(f, &[v64.into()], "").unwrap();
                        }
                        _ if self.is_builtin_string(arg_ty) => {
                            let str_val = self.get_value(arg_val);
                            let (ptr, len) = self.extract_str_ptr_len(str_val);
                            let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
                            let fn_ty = self
                                .ctx
                                .void_type()
                                .fn_type(&[ptr_ty.into(), i64_ty.into()], false);
                            let f = self
                                .module
                                .get_function("__gruel_dbg_str_noln")
                                .unwrap_or_else(|| {
                                    self.module
                                        .add_function("__gruel_dbg_str_noln", fn_ty, None)
                                });
                            self.builder
                                .build_call(f, &[ptr.into(), len.into()], "")
                                .unwrap();
                        }
                        _ => {
                            unreachable!("@dbg: unsupported type {:?}", arg_ty.kind());
                        }
                    }
                }
                self.builder.build_call(newline_fn, &[], "").unwrap();
                None
            }

            // ---- Pointer operations ----
            IntrinsicId::PtrRead | IntrinsicId::PtrReadVolatile => {
                let ptr_val = args[0];
                let ptr = self.get_value(ptr_val).into_pointer_value();
                let result_llvm_ty = gruel_type_to_llvm(ty, self.ctx, self.type_pool)
                    .expect("ptr_read must return a non-void type");
                let loaded = self
                    .builder
                    .build_load(result_llvm_ty, ptr, "ptrrd")
                    .unwrap();
                if matches!(id, IntrinsicId::PtrReadVolatile) {
                    loaded
                        .as_instruction_value()
                        .expect("load returns an instruction")
                        .set_volatile(true)
                        .expect("set_volatile on load");
                }
                Some(loaded)
            }
            IntrinsicId::PtrWrite | IntrinsicId::PtrWriteVolatile => {
                let ptr_val = args[0];
                let written_val = args[1];
                let ptr = self.get_value(ptr_val).into_pointer_value();
                let v = self.get_value(written_val);
                let store = self.builder.build_store(ptr, v).unwrap();
                if matches!(id, IntrinsicId::PtrWriteVolatile) {
                    store.set_volatile(true).expect("set_volatile on store");
                }
                None
            }
            IntrinsicId::PtrOffset => {
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
                let result_ptr = if let Some(elem_llvm) =
                    gruel_type_to_llvm(pointee_ty, self.ctx, self.type_pool)
                {
                    // GEP advances by `offset * sizeof(elem)` automatically.
                    unsafe {
                        self.builder
                            .build_gep(elem_llvm, ptr, &[offset], "gep")
                            .unwrap()
                    }
                } else {
                    ptr // zero-sized pointee — offset has no effect
                };
                Some(result_ptr.into())
            }
            IntrinsicId::PtrToInt => {
                let ptr_val = args[0];
                let ptr = self.get_value(ptr_val).into_pointer_value();
                let i64_ty = self.ctx.i64_type();
                Some(
                    self.builder
                        .build_ptr_to_int(ptr, i64_ty, "p2i")
                        .unwrap()
                        .into(),
                )
            }
            IntrinsicId::IntToPtr => {
                let addr_val = args[0];
                let addr = self.get_value(addr_val).into_int_value();
                let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
                Some(
                    self.builder
                        .build_int_to_ptr(addr, ptr_ty, "i2p")
                        .unwrap()
                        .into(),
                )
            }
            IntrinsicId::NullPtr => {
                let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
                Some(ptr_ty.const_null().into())
            }
            IntrinsicId::IsNull => {
                let ptr_val = args[0];
                let ptr = self.get_value(ptr_val).into_pointer_value();
                let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
                let null = ptr_ty.const_null();
                Some(
                    self.builder
                        .build_int_compare(
                            inkwell::IntPredicate::EQ,
                            self.builder
                                .build_ptr_to_int(ptr, self.ctx.i64_type(), "p2i_lhs")
                                .unwrap(),
                            self.builder
                                .build_ptr_to_int(null, self.ctx.i64_type(), "p2i_rhs")
                                .unwrap(),
                            "isnull",
                        )
                        .unwrap()
                        .into(),
                )
            }
            IntrinsicId::PtrCopy => {
                let dst_val = args[0];
                let src_val = args[1];
                let count_val = args[2];
                let dst = self.get_value(dst_val).into_pointer_value();
                let src = self.get_value(src_val).into_pointer_value();
                let count = self.get_value(count_val).into_int_value();

                // Determine pointee type to compute byte size
                let dst_gruel_ty = self.cfg.get_inst(dst_val).ty;
                let pointee_ty = match dst_gruel_ty.kind() {
                    TypeKind::PtrMut(id) => self.type_pool.ptr_mut_def(id),
                    _ => Type::U8, // fallback
                };

                let i64_ty = self.ctx.i64_type();
                let byte_count = if let Some(elem_llvm) =
                    gruel_type_to_llvm(pointee_ty, self.ctx, self.type_pool)
                {
                    let elem_size = elem_llvm.size_of().unwrap();
                    self.builder
                        .build_int_mul(count, elem_size, "nbytes")
                        .unwrap()
                } else {
                    // Zero-sized type — nothing to copy
                    i64_ty.const_zero()
                };

                // Emit llvm.memcpy.p0.p0.i64
                let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
                let memcpy_ty = self.ctx.void_type().fn_type(
                    &[
                        ptr_ty.into(),
                        ptr_ty.into(),
                        i64_ty.into(),
                        self.ctx.bool_type().into(),
                    ],
                    false,
                );
                let memcpy_fn = self
                    .module
                    .get_function("llvm.memcpy.p0.p0.i64")
                    .unwrap_or_else(|| {
                        self.module
                            .add_function("llvm.memcpy.p0.p0.i64", memcpy_ty, None)
                    });
                self.builder
                    .build_call(
                        memcpy_fn,
                        &[
                            dst.into(),
                            src.into(),
                            byte_count.into(),
                            self.ctx.bool_type().const_zero().into(), // not volatile
                        ],
                        "",
                    )
                    .unwrap();
                None
            }

            // ---- Address-of (raw pointer to any lvalue) ----
            IntrinsicId::Raw | IntrinsicId::RawMut => {
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
                        let place = *place;
                        let elem_ty = lvalue_inst.ty;
                        self.build_place_gep_chain(&place, elem_ty).map(Into::into)
                    }
                    // ADR-0063: `Ptr(T)::from(&x)` lowers via ADR-0062's
                    // MakeRef, which already produces the alloca pointer.
                    // Pass it through unchanged.
                    CfgInstData::MakeRef { .. } => Some(self.get_value(lvalue_val)),
                    _ => {
                        // Fallback: return a null pointer.
                        gruel_type_to_llvm(ty, self.ctx, self.type_pool).map(|t| t.const_zero())
                    }
                }
            }

            // ---- String parsing intrinsics ----
            IntrinsicId::ParseI32
            | IntrinsicId::ParseI64
            | IntrinsicId::ParseU32
            | IntrinsicId::ParseU64 => {
                // @parse_*(s: String) -> integer
                // Extract ptr and len from the String struct, then call __gruel_parse_*.
                // The runtime symbol comes from the registry (def.runtime_fn).
                let runtime_fn = lookup_by_name(name_str)
                    .and_then(|d| d.runtime_fn)
                    .expect("parse_* intrinsics declare a runtime symbol");
                let is_32_bit = matches!(id, IntrinsicId::ParseI32 | IntrinsicId::ParseU32);
                let str_val = self.get_value(args[0]);
                let (ptr, len) = self.extract_str_ptr_len(str_val);
                let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.ctx.i64_type();
                let fn_ty_ret = if is_32_bit {
                    self.ctx
                        .i32_type()
                        .fn_type(&[ptr_ty.into(), i64_ty.into()], false)
                } else {
                    i64_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false)
                };
                let f = self
                    .module
                    .get_function(runtime_fn)
                    .unwrap_or_else(|| self.module.add_function(runtime_fn, fn_ty_ret, None));
                let result = self
                    .builder
                    .build_call(f, &[ptr.into(), len.into()], "parsed")
                    .unwrap();
                result.try_as_basic_value().basic()
            }

            // ---- Read line from stdin ----
            IntrinsicId::ReadLine => {
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
                let runtime_fn = lookup_by_name("read_line")
                    .and_then(|d| d.runtime_fn)
                    .expect("read_line has a runtime symbol");
                let f = self
                    .module
                    .get_function(runtime_fn)
                    .unwrap_or_else(|| self.module.add_function(runtime_fn, fn_ty, None));
                self.builder.build_call(f, &[sret_ptr.into()], "").unwrap();

                // Load the String struct from the sret alloca.
                Some(
                    self.builder
                        .build_load(str_llvm_ty, sret_ptr, "rl_str")
                        .unwrap(),
                )
            }

            // ---- Raw syscall ----
            IntrinsicId::Syscall => {
                let i64_ty = self.ctx.i64_type();
                // Build argument values (all u64, first is syscall number)
                let arg_vals: Vec<_> = args
                    .iter()
                    .map(|a| self.get_value(*a).into_int_value())
                    .collect();
                let num_args = arg_vals.len(); // 1..=7 (syscall_num + up to 6 args)

                let triple = TargetMachine::get_default_triple();
                let triple_str = triple.as_str().to_string_lossy();
                let is_aarch64 =
                    triple_str.starts_with("aarch64") || triple_str.starts_with("arm64");

                let (asm_str, constraints) = if is_aarch64 {
                    // aarch64 syscall convention:
                    //   x8 = syscall number
                    //   x0, x1, x2, x3, x4, x5 = arg1..arg6
                    //   return value in x0
                    match num_args {
                        1 => ("svc #0".to_string(), "={x0},{x8},~{memory}".to_string()),
                        2 => (
                            "svc #0".to_string(),
                            "={x0},{x8},{x0},~{memory}".to_string(),
                        ),
                        3 => (
                            "svc #0".to_string(),
                            "={x0},{x8},{x0},{x1},~{memory}".to_string(),
                        ),
                        4 => (
                            "svc #0".to_string(),
                            "={x0},{x8},{x0},{x1},{x2},~{memory}".to_string(),
                        ),
                        5 => (
                            "svc #0".to_string(),
                            "={x0},{x8},{x0},{x1},{x2},{x3},~{memory}".to_string(),
                        ),
                        6 => (
                            "svc #0".to_string(),
                            "={x0},{x8},{x0},{x1},{x2},{x3},{x4},~{memory}".to_string(),
                        ),
                        7 => (
                            "svc #0".to_string(),
                            "={x0},{x8},{x0},{x1},{x2},{x3},{x4},{x5},~{memory}".to_string(),
                        ),
                        _ => unreachable!("syscall validated to 1-7 args by sema"),
                    }
                } else {
                    // x86_64 syscall convention:
                    //   rax = syscall number
                    //   rdi, rsi, rdx, r10, r8, r9 = arg1..arg6
                    //   return value in rax
                    //   rcx and r11 are clobbered by the kernel
                    match num_args {
                        1 => ("syscall".to_string(), "={rax},{rax},~{rcx},~{r11},~{memory}".to_string()),
                        2 => ("syscall".to_string(), "={rax},{rax},{rdi},~{rcx},~{r11},~{memory}".to_string()),
                        3 => ("syscall".to_string(), "={rax},{rax},{rdi},{rsi},~{rcx},~{r11},~{memory}".to_string()),
                        4 => ("syscall".to_string(), "={rax},{rax},{rdi},{rsi},{rdx},~{rcx},~{r11},~{memory}".to_string()),
                        5 => ("syscall".to_string(), "={rax},{rax},{rdi},{rsi},{rdx},{r10},~{rcx},~{r11},~{memory}".to_string()),
                        6 => ("syscall".to_string(), "={rax},{rax},{rdi},{rsi},{rdx},{r10},{r8},~{rcx},~{r11},~{memory}".to_string()),
                        7 => ("syscall".to_string(), "={rax},{rax},{rdi},{rsi},{rdx},{r10},{r8},{r9},~{rcx},~{r11},~{memory}".to_string()),
                        _ => unreachable!("syscall validated to 1-7 args by sema"),
                    }
                };

                // Build the function type: (i64, ...) -> i64
                let param_types: Vec<inkwell::types::BasicMetadataTypeEnum> =
                    vec![i64_ty.into(); num_args];
                let fn_ty = i64_ty.fn_type(&param_types, false);

                let asm_val = self.ctx.create_inline_asm(
                    fn_ty,
                    asm_str,
                    constraints,
                    true,  // side effects
                    true,  // align stack
                    None,  // default dialect (AT&T)
                    false, // can_throw
                );

                let call_args: Vec<BasicMetadataValueEnum> =
                    arg_vals.iter().map(|v| (*v).into()).collect();
                let result = self
                    .builder
                    .build_indirect_call(fn_ty, asm_val, &call_args, "syscall_ret")
                    .unwrap();
                result.try_as_basic_value().basic()
            }

            // ADR-0064: bounds-checked slice indexing read.
            //
            // `args = [slice, index]`. The slice is the `{ptr, i64}` aggregate;
            // we extract `ptr` and `len`, runtime-check `index < len`, then
            // GEP+load the element.
            IntrinsicId::SliceIndexRead => {
                use inkwell::IntPredicate;
                let slice_val = self.get_value(args[0]).into_struct_value();
                let raw_idx = self.get_value(args[1]).into_int_value();
                let i64_ty = self.ctx.i64_type();
                let bits = raw_idx.get_type().get_bit_width();
                let idx_i64 = if bits < 64 {
                    self.builder
                        .build_int_z_extend(raw_idx, i64_ty, "sidx")
                        .unwrap()
                } else if bits > 64 {
                    self.builder
                        .build_int_truncate(raw_idx, i64_ty, "sidx")
                        .unwrap()
                } else {
                    raw_idx
                };
                let data_ptr = self
                    .builder
                    .build_extract_value(slice_val, 0, "sptr")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_extract_value(slice_val, 1, "slen")
                    .unwrap()
                    .into_int_value();
                let in_bounds = self
                    .builder
                    .build_int_compare(IntPredicate::ULT, idx_i64, len, "sob")
                    .unwrap();
                let in_bounds = self.build_expect_i1(in_bounds, true);
                let current_fn = self
                    .builder
                    .get_insert_block()
                    .unwrap()
                    .get_parent()
                    .unwrap();
                let ok_bb = self.ctx.append_basic_block(current_fn, "sok");
                let oob_bb = self.ctx.append_basic_block(current_fn, "soob");
                self.builder
                    .build_conditional_branch(in_bounds, ok_bb, oob_bb)
                    .unwrap();
                self.builder.position_at_end(oob_bb);
                let check_fn = self.get_or_declare_noreturn_fn("__gruel_bounds_check");
                self.builder.build_call(check_fn, &[], "").unwrap();
                self.builder.build_unreachable().unwrap();
                self.builder.position_at_end(ok_bb);

                let elem_ty = ty;
                let elem_llvm_ty = gruel_type_to_llvm(elem_ty, self.ctx, self.type_pool);
                match elem_llvm_ty {
                    Some(et) => {
                        let elem_ptr = unsafe {
                            self.builder
                                .build_gep(et, data_ptr, &[idx_i64], "sgep")
                                .unwrap()
                        };
                        let v = self.builder.build_load(et, elem_ptr, "sload").unwrap();
                        Some(v)
                    }
                    None => None, // zero-sized element
                }
            }
            // ADR-0064: bounds-checked slice indexing write.
            // `args = [slice, index, value]`.
            IntrinsicId::SliceIndexWrite => {
                use inkwell::IntPredicate;
                let slice_val = self.get_value(args[0]).into_struct_value();
                let raw_idx = self.get_value(args[1]).into_int_value();
                let value_inst_ty = self.cfg.get_inst(args[2]).ty;
                let i64_ty = self.ctx.i64_type();
                let bits = raw_idx.get_type().get_bit_width();
                let idx_i64 = if bits < 64 {
                    self.builder
                        .build_int_z_extend(raw_idx, i64_ty, "sidx")
                        .unwrap()
                } else if bits > 64 {
                    self.builder
                        .build_int_truncate(raw_idx, i64_ty, "sidx")
                        .unwrap()
                } else {
                    raw_idx
                };
                let data_ptr = self
                    .builder
                    .build_extract_value(slice_val, 0, "sptr")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_extract_value(slice_val, 1, "slen")
                    .unwrap()
                    .into_int_value();
                let in_bounds = self
                    .builder
                    .build_int_compare(IntPredicate::ULT, idx_i64, len, "sob")
                    .unwrap();
                let in_bounds = self.build_expect_i1(in_bounds, true);
                let current_fn = self
                    .builder
                    .get_insert_block()
                    .unwrap()
                    .get_parent()
                    .unwrap();
                let ok_bb = self.ctx.append_basic_block(current_fn, "sok");
                let oob_bb = self.ctx.append_basic_block(current_fn, "soob");
                self.builder
                    .build_conditional_branch(in_bounds, ok_bb, oob_bb)
                    .unwrap();
                self.builder.position_at_end(oob_bb);
                let check_fn = self.get_or_declare_noreturn_fn("__gruel_bounds_check");
                self.builder.build_call(check_fn, &[], "").unwrap();
                self.builder.build_unreachable().unwrap();
                self.builder.position_at_end(ok_bb);

                if let Some(elem_llvm_ty) =
                    gruel_type_to_llvm(value_inst_ty, self.ctx, self.type_pool)
                {
                    let elem_ptr = unsafe {
                        self.builder
                            .build_gep(elem_llvm_ty, data_ptr, &[idx_i64], "sgep")
                            .unwrap()
                    };
                    let v = self.get_value(args[2]);
                    self.builder.build_store(elem_ptr, v).unwrap();
                }
                None
            }

            // ADR-0064: extract the data pointer from a slice. The result
            // is a `Ptr(T)` (or `MutPtr(T)`) backed by the same LLVM `ptr`.
            IntrinsicId::SlicePtr | IntrinsicId::SlicePtrMut => {
                let recv_val = self.get_value(args[0]).into_struct_value();
                let ptr = self
                    .builder
                    .build_extract_value(recv_val, 0, "slice_ptr")
                    .unwrap()
                    .into_pointer_value();
                Some(ptr.into())
            }
            // ADR-0064: build a slice from (raw_ptr, length).
            IntrinsicId::PartsToSlice | IntrinsicId::PartsToMutSlice => {
                let ptr_val = self.get_value(args[0]).into_pointer_value();
                let raw_n = self.get_value(args[1]).into_int_value();
                let i64_ty = self.ctx.i64_type();
                let bits = raw_n.get_type().get_bit_width();
                let n_i64 = if bits < 64 {
                    self.builder.build_int_z_extend(raw_n, i64_ty, "n").unwrap()
                } else if bits > 64 {
                    self.builder.build_int_truncate(raw_n, i64_ty, "n").unwrap()
                } else {
                    raw_n
                };
                let slice_llvm_ty = gruel_type_to_llvm(ty, self.ctx, self.type_pool)
                    .expect("slice has LLVM lowering")
                    .into_struct_type();
                let undef = slice_llvm_ty.get_undef();
                let with_ptr = self
                    .builder
                    .build_insert_value(undef, ptr_val, 0, "p2s_p")
                    .unwrap();
                let agg = self
                    .builder
                    .build_insert_value(with_ptr, n_i64, 1, "p2s")
                    .unwrap();
                Some(agg.into_struct_value().into())
            }

            // ADR-0064: Slice operations.
            //
            // A slice is lowered to the LLVM aggregate `{ ptr, i64 }` (see
            // `gruel_type_to_llvm`). `len` is field index 1; `is_empty` is
            // `len == 0`.
            IntrinsicId::SliceLen => {
                let recv_val = self.get_value(args[0]).into_struct_value();
                let len = self
                    .builder
                    .build_extract_value(recv_val, 1, "slice_len")
                    .unwrap()
                    .into_int_value();
                Some(len.into())
            }
            IntrinsicId::SliceIsEmpty => {
                let recv_val = self.get_value(args[0]).into_struct_value();
                let len = self
                    .builder
                    .build_extract_value(recv_val, 1, "slice_len")
                    .unwrap()
                    .into_int_value();
                let zero = self.ctx.i64_type().const_zero();
                let is_empty = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, len, zero, "slice_is_empty")
                    .unwrap();
                Some(is_empty.into())
            }

            // ============================================================
            // ADR-0066: Vec(T) operations.
            //
            // Vec(T) lowers to `{ T*, i64, i64 }`. Receivers passed as
            // borrow/inout are LLVM `ptr` to the aggregate; we load fields
            // from those pointers. By-value receivers are aggregate values.
            // ============================================================
            IntrinsicId::VecNew => self.translate_vec_new(ty),
            IntrinsicId::VecWithCapacity => self.translate_vec_with_capacity(ty, args),
            IntrinsicId::VecLen => Some(self.translate_vec_field_load(args[0], 1, "vec_len")),
            IntrinsicId::VecCapacity => Some(self.translate_vec_field_load(args[0], 2, "vec_cap")),
            IntrinsicId::VecIsEmpty => {
                let len = self
                    .translate_vec_field_load(args[0], 1, "vec_len")
                    .into_int_value();
                let zero = self.ctx.i64_type().const_zero();
                let is_empty = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, len, zero, "vec_is_empty")
                    .unwrap();
                Some(is_empty.into())
            }
            IntrinsicId::VecPush => {
                self.translate_vec_push(args);
                None
            }
            IntrinsicId::VecPop => Some(self.translate_vec_pop(args, ty)),
            IntrinsicId::VecClear => {
                self.translate_vec_clear(args);
                None
            }
            IntrinsicId::VecReserve => {
                self.translate_vec_reserve(args);
                None
            }
            IntrinsicId::VecIndexRead => Some(self.translate_vec_index_read(args, ty)),
            IntrinsicId::VecIndexWrite => {
                self.translate_vec_index_write(args);
                None
            }
            IntrinsicId::VecPtr | IntrinsicId::VecPtrMut => {
                Some(self.translate_vec_field_load(args[0], 0, "vec_ptr"))
            }
            IntrinsicId::VecTerminatedPtr => Some(self.translate_vec_terminated_ptr(args)),
            IntrinsicId::VecClone => Some(self.translate_vec_clone(args, ty)),
            IntrinsicId::VecLiteral => Some(self.translate_vec_literal(args, ty)),
            IntrinsicId::VecRepeat => Some(self.translate_vec_repeat(args, ty)),
            IntrinsicId::VecDispose => {
                self.translate_vec_dispose(args);
                None
            }
            IntrinsicId::PartsToVec => Some(self.translate_parts_to_vec(args, ty)),

            // ADR-0072: @utf8_validate(s) — call __gruel_utf8_validate(ptr, len) -> u8,
            // convert to bool.
            IntrinsicId::Utf8Validate => Some(self.translate_utf8_validate(args)),

            // ADR-0072: @vec_from_c_str(p) — call __gruel_vec_from_c_str(out, p)
            // via sret, return the Vec(u8) aggregate.
            IntrinsicId::VecFromCStr => Some(self.translate_vec_from_c_str(args)),

            // ---- Fallback: return zero value for unimplemented intrinsics ----
            _ => gruel_type_to_llvm(ty, self.ctx, self.type_pool).map(|t| t.const_zero()),
        }
    }

    /// Codegen for `@vec_from_c_str(p: Ptr(u8)) -> Vec(u8)` (ADR-0072).
    /// Calls `__gruel_vec_from_c_str(out: *mut VecU8Result, p: *const u8)`
    /// via the sret convention and loads the resulting Vec(u8) aggregate.
    fn translate_vec_from_c_str(&mut self, args: &[CfgValue]) -> BasicValueEnum<'ctx> {
        let p = self.get_value(args[0]).into_pointer_value();
        let agg_ty = self.vec_agg_type();
        let sret_slot = self.build_entry_alloca(agg_ty.into(), "vfcs_sret");
        let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
        let fn_ty = self
            .ctx
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let callee = self
            .module
            .get_function("__gruel_vec_from_c_str")
            .unwrap_or_else(|| {
                self.module
                    .add_function("__gruel_vec_from_c_str", fn_ty, None)
            });
        self.builder
            .build_call(callee, &[sret_slot.into(), p.into()], "")
            .unwrap();
        self.builder
            .build_load(agg_ty, sret_slot, "vfcs_load")
            .unwrap()
    }

    /// Codegen for `@utf8_validate(s: borrow Slice(u8)) -> bool` (ADR-0072).
    /// Extracts (ptr, len) from the slice and calls
    /// `__gruel_utf8_validate(ptr, len) -> u8`, then truncates to i1.
    fn translate_utf8_validate(&mut self, args: &[CfgValue]) -> BasicValueEnum<'ctx> {
        let slice_val = self.get_value(args[0]).into_struct_value();
        let ptr = self
            .builder
            .build_extract_value(slice_val, 0, "u8v_ptr")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_extract_value(slice_val, 1, "u8v_len")
            .unwrap()
            .into_int_value();
        let i64_ty = self.ctx.i64_type();
        let i8_ty = self.ctx.i8_type();
        let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
        let fn_ty = i8_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false);
        let callee = self
            .module
            .get_function("__gruel_utf8_validate")
            .unwrap_or_else(|| {
                self.module
                    .add_function("__gruel_utf8_validate", fn_ty, None)
            });
        let result = self
            .builder
            .build_call(callee, &[ptr.into(), len.into()], "utf8_valid")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_int_value();
        // u8 → bool (i1): nonzero is true.
        let zero = i8_ty.const_zero();
        let cmp = self
            .builder
            .build_int_compare(IntPredicate::NE, result, zero, "u8v_b")
            .unwrap();
        cmp.into()
    }

    /// Element type from a `Vec(T)` value.
    fn vec_element_type(&self, vec_ty: Type) -> Type {
        match vec_ty.kind() {
            TypeKind::Vec(id) => self.type_pool.vec_def(id),
            _ => unreachable!("vec_element_type called with non-Vec"),
        }
    }

    /// Get a pointer to the Vec aggregate. Inout/Borrow CFG args are passed
    /// pre-load (the codegen call path materializes a pointer in `call_args`),
    /// but `Intrinsic` args don't go through that path — the Vec value is
    /// available as either a pointer (when the source has a stable storage
    /// slot) or an aggregate value (when materialized inline). This helper
    /// covers both: prefer the source's existing storage; fall back to a
    /// temporary alloca + store + return its pointer.
    fn vec_recv_ptr(&mut self, recv: CfgValue) -> inkwell::values::PointerValue<'ctx> {
        let inst = self.cfg.get_inst(recv).clone();
        match inst.data {
            CfgInstData::Load { slot } => {
                self.locals[slot as usize].expect("vec receiver: slot not yet allocated")
            }
            CfgInstData::Param { index } if self.cfg.is_param_by_ref(index) => {
                let llvm_idx = self.slot_to_llvm_param[index as usize];
                self.fn_value
                    .get_nth_param(llvm_idx)
                    .expect("param slot out of range")
                    .into_pointer_value()
            }
            _ => {
                let v = self.get_value(recv);
                let agg_ty = self.vec_agg_type();
                let tmp = self.build_entry_alloca(agg_ty.into(), "vec_recv_tmp");
                self.builder.build_store(tmp, v).unwrap();
                tmp
            }
        }
    }

    /// Load a single field of a Vec aggregate via its pointer.
    fn translate_vec_field_load(
        &mut self,
        recv: CfgValue,
        field: u32,
        name: &str,
    ) -> BasicValueEnum<'ctx> {
        let recv_ptr = self.vec_recv_ptr(recv);
        let agg_ty = self.vec_agg_type();
        let field_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, field, name)
            .unwrap();
        let field_llvm_ty: BasicTypeEnum<'ctx> = match field {
            0 => self.ctx.ptr_type(inkwell::AddressSpace::default()).into(),
            _ => self.ctx.i64_type().into(),
        };
        self.builder
            .build_load(field_llvm_ty, field_ptr, name)
            .unwrap()
    }

    /// LLVM type of a Vec aggregate (independent of element type).
    fn vec_agg_type(&self) -> inkwell::types::StructType<'ctx> {
        let ptr = self.ctx.ptr_type(inkwell::AddressSpace::default()).into();
        let i64_ty = self.ctx.i64_type().into();
        self.ctx.struct_type(&[ptr, i64_ty, i64_ty], false)
    }

    /// Build a `Vec(T)` value with given fields.
    fn vec_pack(
        &mut self,
        ptr: inkwell::values::PointerValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
        cap: inkwell::values::IntValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let agg = self.vec_agg_type().get_undef();
        let with_ptr = self
            .builder
            .build_insert_value(agg, ptr, 0, "vec_p")
            .unwrap();
        let with_len = self
            .builder
            .build_insert_value(with_ptr, len, 1, "vec_l")
            .unwrap();
        let full = self
            .builder
            .build_insert_value(with_len, cap, 2, "vec_c")
            .unwrap();
        full.into_struct_value().into()
    }

    /// Get the byte size of `T` for `Vec(T)`-typed `vec_ty`.
    fn vec_elem_size(&self, vec_ty: Type) -> inkwell::values::IntValue<'ctx> {
        let elem_ty = self.vec_element_type(vec_ty);
        let i64_ty = self.ctx.i64_type();
        match gruel_type_to_llvm(elem_ty, self.ctx, self.type_pool) {
            Some(llvm_ty) => llvm_ty.size_of().unwrap_or_else(|| i64_ty.const_zero()),
            None => i64_ty.const_zero(),
        }
    }

    /// `__gruel_alloc(size, align) -> *u8`.
    fn vec_alloc_fn(&mut self) -> inkwell::values::FunctionValue<'ctx> {
        let i64_ty = self.ctx.i64_type();
        let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
        let fn_ty = ptr_ty.fn_type(&[i64_ty.into(), i64_ty.into()], false);
        self.module
            .get_function("__gruel_alloc")
            .unwrap_or_else(|| self.module.add_function("__gruel_alloc", fn_ty, None))
    }

    fn vec_realloc_fn(&mut self) -> inkwell::values::FunctionValue<'ctx> {
        let i64_ty = self.ctx.i64_type();
        let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
        let fn_ty = ptr_ty.fn_type(
            &[ptr_ty.into(), i64_ty.into(), i64_ty.into(), i64_ty.into()],
            false,
        );
        self.module
            .get_function("__gruel_realloc")
            .unwrap_or_else(|| self.module.add_function("__gruel_realloc", fn_ty, None))
    }

    fn vec_free_fn(&mut self) -> inkwell::values::FunctionValue<'ctx> {
        let i64_ty = self.ctx.i64_type();
        let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
        let fn_ty = self
            .ctx
            .void_type()
            .fn_type(&[ptr_ty.into(), i64_ty.into(), i64_ty.into()], false);
        self.module
            .get_function("__gruel_free")
            .unwrap_or_else(|| self.module.add_function("__gruel_free", fn_ty, None))
    }

    /// `Vec::new()` — empty Vec, ptr=null, len=0, cap=0.
    fn translate_vec_new(&mut self, _ty: Type) -> Option<BasicValueEnum<'ctx>> {
        let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.ctx.i64_type();
        let null_ptr = ptr_ty.const_null();
        let zero = i64_ty.const_zero();
        Some(self.vec_pack(null_ptr, zero, zero))
    }

    /// `Vec::with_capacity(n)` — alloc n*sizeof(T), ptr=alloc, len=0, cap=n.
    fn translate_vec_with_capacity(
        &mut self,
        vec_ty: Type,
        args: &[CfgValue],
    ) -> Option<BasicValueEnum<'ctx>> {
        let n_raw = self.get_value(args[0]).into_int_value();
        let i64_ty = self.ctx.i64_type();
        let n = self.zext_to_i64(n_raw);
        let elem_size = self.vec_elem_size(vec_ty);
        let bytes = self
            .builder
            .build_int_mul(n, elem_size, "vec_alloc_bytes")
            .unwrap();
        let align = i64_ty.const_int(8, false);
        let alloc_fn = self.vec_alloc_fn();
        let raw_ptr = self
            .builder
            .build_call(alloc_fn, &[bytes.into(), align.into()], "vec_alloc")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();
        // If n == 0, alloc may return null; that's fine for an empty Vec.
        Some(self.vec_pack(raw_ptr, i64_ty.const_zero(), n))
    }

    fn zext_to_i64(&self, v: inkwell::values::IntValue<'ctx>) -> inkwell::values::IntValue<'ctx> {
        let i64_ty = self.ctx.i64_type();
        let bits = v.get_type().get_bit_width();
        if bits < 64 {
            self.builder.build_int_z_extend(v, i64_ty, "z").unwrap()
        } else if bits > 64 {
            self.builder.build_int_truncate(v, i64_ty, "tr").unwrap()
        } else {
            v
        }
    }

    /// `v.push(x)` — grow if cap==len, store x at ptr[len], len+=1.
    fn translate_vec_push(&mut self, args: &[CfgValue]) {
        let recv_ptr = self.vec_recv_ptr(args[0]);
        let val = self.get_value(args[1]);
        let agg_ty = self.vec_agg_type();
        let i64_ty = self.ctx.i64_type();
        let one = i64_ty.const_int(1, false);

        // Determine element type from arg's CFG type. The pushed value's
        // type is T.
        let val_gruel_ty = self.cfg.get_inst(args[1]).ty;
        let elem_size = match gruel_type_to_llvm(val_gruel_ty, self.ctx, self.type_pool) {
            Some(t) => t.size_of().unwrap_or_else(|| i64_ty.const_zero()),
            None => i64_ty.const_zero(),
        };

        // Load current ptr/len/cap.
        let len_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, 1, "len_ptr")
            .unwrap();
        let cap_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, 2, "cap_ptr")
            .unwrap();
        let buf_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, 0, "buf_ptr")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_ty, len_ptr, "len")
            .unwrap()
            .into_int_value();
        let cap = self
            .builder
            .build_load(i64_ty, cap_ptr, "cap")
            .unwrap()
            .into_int_value();

        // If cap == len, grow.
        let needs_grow = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, len, cap, "needs_grow")
            .unwrap();
        let grow_bb = self.ctx.append_basic_block(self.fn_value, "vec_push_grow");
        let after_bb = self
            .ctx
            .append_basic_block(self.fn_value, "vec_push_after_grow");
        self.builder
            .build_conditional_branch(needs_grow, grow_bb, after_bb)
            .unwrap();

        // Grow: new_cap = max(cap*2, 4); realloc.
        self.builder.position_at_end(grow_bb);
        let two = i64_ty.const_int(2, false);
        let four = i64_ty.const_int(4, false);
        let cap_x2 = self.builder.build_int_mul(cap, two, "cap_x2").unwrap();
        let cap_ge_4 = self
            .builder
            .build_int_compare(inkwell::IntPredicate::UGE, cap_x2, four, "cap_ge_4")
            .unwrap();
        let new_cap = self
            .builder
            .build_select(cap_ge_4, cap_x2, four, "new_cap")
            .unwrap()
            .into_int_value();
        let old_bytes = self
            .builder
            .build_int_mul(cap, elem_size, "old_bytes")
            .unwrap();
        let new_bytes = self
            .builder
            .build_int_mul(new_cap, elem_size, "new_bytes")
            .unwrap();
        let align = i64_ty.const_int(8, false);
        let realloc_fn = self.vec_realloc_fn();
        let old_ptr = self
            .builder
            .build_load(
                self.ctx.ptr_type(inkwell::AddressSpace::default()),
                buf_ptr,
                "old_ptr",
            )
            .unwrap()
            .into_pointer_value();
        let new_ptr = self
            .builder
            .build_call(
                realloc_fn,
                &[
                    old_ptr.into(),
                    old_bytes.into(),
                    new_bytes.into(),
                    align.into(),
                ],
                "vec_realloc",
            )
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();
        self.builder.build_store(buf_ptr, new_ptr).unwrap();
        self.builder.build_store(cap_ptr, new_cap).unwrap();
        self.builder.build_unconditional_branch(after_bb).unwrap();

        // After-grow: store value at ptr[len], len+=1.
        self.builder.position_at_end(after_bb);
        let cur_buf = self
            .builder
            .build_load(
                self.ctx.ptr_type(inkwell::AddressSpace::default()),
                buf_ptr,
                "cur_buf",
            )
            .unwrap()
            .into_pointer_value();
        let elem_llvm = match gruel_type_to_llvm(val_gruel_ty, self.ctx, self.type_pool) {
            Some(t) => t,
            None => return,
        };
        let slot = unsafe {
            self.builder
                .build_in_bounds_gep(elem_llvm, cur_buf, &[len], "push_slot")
                .unwrap()
        };
        self.builder.build_store(slot, val).unwrap();
        let new_len = self.builder.build_int_add(len, one, "new_len").unwrap();
        self.builder.build_store(len_ptr, new_len).unwrap();
    }

    /// `v.pop()` — len-=1, load from ptr[new_len].
    fn translate_vec_pop(&mut self, args: &[CfgValue], elem_ty: Type) -> BasicValueEnum<'ctx> {
        let recv_ptr = self.vec_recv_ptr(args[0]);
        let agg_ty = self.vec_agg_type();
        let i64_ty = self.ctx.i64_type();
        let one = i64_ty.const_int(1, false);
        let len_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, 1, "len_ptr")
            .unwrap();
        let buf_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, 0, "buf_ptr")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_ty, len_ptr, "len")
            .unwrap()
            .into_int_value();
        // For v1: panic if empty (no Option wrapping). The pop intrinsic
        // returns the bare element; an empty pop traps via __gruel_panic.
        let zero = i64_ty.const_zero();
        let is_empty = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, len, zero, "pop_empty")
            .unwrap();
        let panic_bb = self.ctx.append_basic_block(self.fn_value, "pop_panic");
        let ok_bb = self.ctx.append_basic_block(self.fn_value, "pop_ok");
        self.builder
            .build_conditional_branch(is_empty, panic_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(panic_bb);
        let panic_fn = self
            .module
            .get_function("__gruel_panic_no_msg")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "__gruel_panic_no_msg",
                    self.ctx.void_type().fn_type(&[], false),
                    None,
                )
            });
        self.builder.build_call(panic_fn, &[], "").unwrap();
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(ok_bb);
        let new_len = self.builder.build_int_sub(len, one, "new_len").unwrap();
        self.builder.build_store(len_ptr, new_len).unwrap();
        let buf = self
            .builder
            .build_load(
                self.ctx.ptr_type(inkwell::AddressSpace::default()),
                buf_ptr,
                "buf",
            )
            .unwrap()
            .into_pointer_value();
        let elem_llvm = match gruel_type_to_llvm(elem_ty, self.ctx, self.type_pool) {
            Some(t) => t,
            None => return self.ctx.i64_type().const_zero().into(),
        };
        let slot = unsafe {
            self.builder
                .build_in_bounds_gep(elem_llvm, buf, &[new_len], "pop_slot")
                .unwrap()
        };
        self.builder.build_load(elem_llvm, slot, "popped").unwrap()
    }

    /// `v.clear()` — drop each live element, then set len = 0.
    fn translate_vec_clear(&mut self, args: &[CfgValue]) {
        let recv_ptr = self.vec_recv_ptr(args[0]);
        let recv_vec_ty = self.cfg.get_inst(args[0]).ty;
        let elem_ty = self.vec_element_type(recv_vec_ty);
        let agg_ty = self.vec_agg_type();
        let i64_ty = self.ctx.i64_type();
        let len_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, 1, "len_ptr")
            .unwrap();
        // Per-element drop loop if T needs drop.
        if drop_names::type_needs_drop(elem_ty, self.type_pool) {
            let buf_ptr = self
                .builder
                .build_struct_gep(agg_ty, recv_ptr, 0, "buf_ptr")
                .unwrap();
            let len = self
                .builder
                .build_load(i64_ty, len_ptr, "clear_len")
                .unwrap()
                .into_int_value();
            let buf = self
                .builder
                .build_load(
                    self.ctx.ptr_type(inkwell::AddressSpace::default()),
                    buf_ptr,
                    "clear_buf",
                )
                .unwrap()
                .into_pointer_value();
            self.emit_vec_drop_loop(buf, len, elem_ty);
        }
        self.builder
            .build_store(len_ptr, i64_ty.const_zero())
            .unwrap();
    }

    /// `v.reserve(n)` — ensure cap >= len + n via realloc.
    fn translate_vec_reserve(&mut self, args: &[CfgValue]) {
        let recv_ptr = self.vec_recv_ptr(args[0]);
        let n_raw = self.get_value(args[1]).into_int_value();
        let n = self.zext_to_i64(n_raw);
        let agg_ty = self.vec_agg_type();
        let i64_ty = self.ctx.i64_type();
        let len_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, 1, "len_ptr")
            .unwrap();
        let cap_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, 2, "cap_ptr")
            .unwrap();
        let buf_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, 0, "buf_ptr")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_ty, len_ptr, "len")
            .unwrap()
            .into_int_value();
        let cap = self
            .builder
            .build_load(i64_ty, cap_ptr, "cap")
            .unwrap()
            .into_int_value();
        let needed = self.builder.build_int_add(len, n, "needed").unwrap();
        let needs_grow = self
            .builder
            .build_int_compare(inkwell::IntPredicate::UGT, needed, cap, "needs_grow")
            .unwrap();
        let grow_bb = self.ctx.append_basic_block(self.fn_value, "reserve_grow");
        let after_bb = self.ctx.append_basic_block(self.fn_value, "reserve_after");
        self.builder
            .build_conditional_branch(needs_grow, grow_bb, after_bb)
            .unwrap();
        self.builder.position_at_end(grow_bb);
        // Element size: the receiver's vec type isn't readily available
        // from here (it's at the receiver's CFG inst's type). Read it.
        let recv_vec_ty = self.cfg.get_inst(args[0]).ty;
        let elem_size = self.vec_elem_size(recv_vec_ty);
        let old_bytes = self.builder.build_int_mul(cap, elem_size, "ob").unwrap();
        let new_bytes = self.builder.build_int_mul(needed, elem_size, "nb").unwrap();
        let align = i64_ty.const_int(8, false);
        let realloc_fn = self.vec_realloc_fn();
        let old = self
            .builder
            .build_load(
                self.ctx.ptr_type(inkwell::AddressSpace::default()),
                buf_ptr,
                "old",
            )
            .unwrap()
            .into_pointer_value();
        let new_buf = self
            .builder
            .build_call(
                realloc_fn,
                &[old.into(), old_bytes.into(), new_bytes.into(), align.into()],
                "rb",
            )
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();
        self.builder.build_store(buf_ptr, new_buf).unwrap();
        self.builder.build_store(cap_ptr, needed).unwrap();
        self.builder.build_unconditional_branch(after_bb).unwrap();
        self.builder.position_at_end(after_bb);
    }

    /// `v[i]` — bounds-check, GEP, load.
    fn translate_vec_index_read(
        &mut self,
        args: &[CfgValue],
        elem_ty: Type,
    ) -> BasicValueEnum<'ctx> {
        let recv_ptr = self.vec_recv_ptr(args[0]);
        let i_raw = self.get_value(args[1]).into_int_value();
        let i = self.zext_to_i64(i_raw);
        let agg_ty = self.vec_agg_type();
        let i64_ty = self.ctx.i64_type();
        let len_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, 1, "len_ptr")
            .unwrap();
        let buf_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, 0, "buf_ptr")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_ty, len_ptr, "len")
            .unwrap()
            .into_int_value();
        let oob = self
            .builder
            .build_int_compare(inkwell::IntPredicate::UGE, i, len, "oob")
            .unwrap();
        let panic_bb = self.ctx.append_basic_block(self.fn_value, "idx_panic");
        let ok_bb = self.ctx.append_basic_block(self.fn_value, "idx_ok");
        self.builder
            .build_conditional_branch(oob, panic_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(panic_bb);
        let bc_fn = self
            .module
            .get_function("__gruel_bounds_check")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "__gruel_bounds_check",
                    self.ctx.void_type().fn_type(&[], false),
                    None,
                )
            });
        self.builder.build_call(bc_fn, &[], "").unwrap();
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(ok_bb);
        let buf = self
            .builder
            .build_load(
                self.ctx.ptr_type(inkwell::AddressSpace::default()),
                buf_ptr,
                "buf",
            )
            .unwrap()
            .into_pointer_value();
        let elem_llvm = match gruel_type_to_llvm(elem_ty, self.ctx, self.type_pool) {
            Some(t) => t,
            None => return self.ctx.i64_type().const_zero().into(),
        };
        let slot = unsafe {
            self.builder
                .build_in_bounds_gep(elem_llvm, buf, &[i], "slot")
                .unwrap()
        };
        self.builder.build_load(elem_llvm, slot, "elem").unwrap()
    }

    /// `v[i] = x`.
    fn translate_vec_index_write(&mut self, args: &[CfgValue]) {
        let recv_ptr = self.vec_recv_ptr(args[0]);
        let i_raw = self.get_value(args[1]).into_int_value();
        let i = self.zext_to_i64(i_raw);
        let val = self.get_value(args[2]);
        let val_ty = self.cfg.get_inst(args[2]).ty;
        let agg_ty = self.vec_agg_type();
        let i64_ty = self.ctx.i64_type();
        let len_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, 1, "len_ptr")
            .unwrap();
        let buf_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, 0, "buf_ptr")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_ty, len_ptr, "len")
            .unwrap()
            .into_int_value();
        let oob = self
            .builder
            .build_int_compare(inkwell::IntPredicate::UGE, i, len, "oob")
            .unwrap();
        let panic_bb = self.ctx.append_basic_block(self.fn_value, "iw_panic");
        let ok_bb = self.ctx.append_basic_block(self.fn_value, "iw_ok");
        self.builder
            .build_conditional_branch(oob, panic_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(panic_bb);
        let bc_fn = self
            .module
            .get_function("__gruel_bounds_check")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "__gruel_bounds_check",
                    self.ctx.void_type().fn_type(&[], false),
                    None,
                )
            });
        self.builder.build_call(bc_fn, &[], "").unwrap();
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(ok_bb);
        let buf = self
            .builder
            .build_load(
                self.ctx.ptr_type(inkwell::AddressSpace::default()),
                buf_ptr,
                "buf",
            )
            .unwrap()
            .into_pointer_value();
        let elem_llvm = match gruel_type_to_llvm(val_ty, self.ctx, self.type_pool) {
            Some(t) => t,
            None => return,
        };
        let slot = unsafe {
            self.builder
                .build_in_bounds_gep(elem_llvm, buf, &[i], "slot")
                .unwrap()
        };
        self.builder.build_store(slot, val).unwrap();
    }

    /// `v.terminated_ptr(s)` — ensure cap > len, write s at ptr[len], return ptr.
    fn translate_vec_terminated_ptr(&mut self, args: &[CfgValue]) -> BasicValueEnum<'ctx> {
        let recv_ptr = self.vec_recv_ptr(args[0]);
        let s = self.get_value(args[1]);
        let s_ty = self.cfg.get_inst(args[1]).ty;
        let agg_ty = self.vec_agg_type();
        let i64_ty = self.ctx.i64_type();
        let one = i64_ty.const_int(1, false);
        let len_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, 1, "len_ptr")
            .unwrap();
        let cap_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, 2, "cap_ptr")
            .unwrap();
        let buf_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, 0, "buf_ptr")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_ty, len_ptr, "len")
            .unwrap()
            .into_int_value();
        let cap = self
            .builder
            .build_load(i64_ty, cap_ptr, "cap")
            .unwrap()
            .into_int_value();
        // Need cap > len. If cap <= len, grow to len+1.
        let needs_grow = self
            .builder
            .build_int_compare(inkwell::IntPredicate::ULE, cap, len, "needs_grow")
            .unwrap();
        let grow_bb = self.ctx.append_basic_block(self.fn_value, "tp_grow");
        let after_bb = self.ctx.append_basic_block(self.fn_value, "tp_after");
        self.builder
            .build_conditional_branch(needs_grow, grow_bb, after_bb)
            .unwrap();
        self.builder.position_at_end(grow_bb);
        let new_cap = self.builder.build_int_add(len, one, "new_cap").unwrap();
        let elem_size = match gruel_type_to_llvm(s_ty, self.ctx, self.type_pool) {
            Some(t) => t.size_of().unwrap_or_else(|| i64_ty.const_zero()),
            None => i64_ty.const_zero(),
        };
        let old_bytes = self.builder.build_int_mul(cap, elem_size, "ob").unwrap();
        let new_bytes = self
            .builder
            .build_int_mul(new_cap, elem_size, "nb")
            .unwrap();
        let align = i64_ty.const_int(8, false);
        let realloc_fn = self.vec_realloc_fn();
        let old = self
            .builder
            .build_load(
                self.ctx.ptr_type(inkwell::AddressSpace::default()),
                buf_ptr,
                "old",
            )
            .unwrap()
            .into_pointer_value();
        let new_buf = self
            .builder
            .build_call(
                realloc_fn,
                &[old.into(), old_bytes.into(), new_bytes.into(), align.into()],
                "rb",
            )
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();
        self.builder.build_store(buf_ptr, new_buf).unwrap();
        self.builder.build_store(cap_ptr, new_cap).unwrap();
        self.builder.build_unconditional_branch(after_bb).unwrap();
        self.builder.position_at_end(after_bb);
        let buf = self
            .builder
            .build_load(
                self.ctx.ptr_type(inkwell::AddressSpace::default()),
                buf_ptr,
                "buf",
            )
            .unwrap()
            .into_pointer_value();
        let elem_llvm = match gruel_type_to_llvm(s_ty, self.ctx, self.type_pool) {
            Some(t) => t,
            None => {
                let null = self
                    .ctx
                    .ptr_type(inkwell::AddressSpace::default())
                    .const_null();
                return null.into();
            }
        };
        let slot = unsafe {
            self.builder
                .build_in_bounds_gep(elem_llvm, buf, &[len], "slot")
                .unwrap()
        };
        self.builder.build_store(slot, s).unwrap();
        buf.into()
    }

    /// `v.clone()` — alloc cap, memcpy len*sizeof(T) bytes.
    fn translate_vec_clone(&mut self, args: &[CfgValue], vec_ty: Type) -> BasicValueEnum<'ctx> {
        let recv_ptr = self.vec_recv_ptr(args[0]);
        let agg_ty = self.vec_agg_type();
        let i64_ty = self.ctx.i64_type();
        let len_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, 1, "len_ptr")
            .unwrap();
        let cap_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, 2, "cap_ptr")
            .unwrap();
        let buf_ptr = self
            .builder
            .build_struct_gep(agg_ty, recv_ptr, 0, "buf_ptr")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_ty, len_ptr, "len")
            .unwrap()
            .into_int_value();
        let cap = self
            .builder
            .build_load(i64_ty, cap_ptr, "cap")
            .unwrap()
            .into_int_value();
        let buf = self
            .builder
            .build_load(
                self.ctx.ptr_type(inkwell::AddressSpace::default()),
                buf_ptr,
                "buf",
            )
            .unwrap()
            .into_pointer_value();
        let elem_size = self.vec_elem_size(vec_ty);
        let alloc_bytes = self
            .builder
            .build_int_mul(cap, elem_size, "alloc_bytes")
            .unwrap();
        let copy_bytes = self
            .builder
            .build_int_mul(len, elem_size, "copy_bytes")
            .unwrap();
        let align = i64_ty.const_int(8, false);
        let alloc_fn = self.vec_alloc_fn();
        let new_buf = self
            .builder
            .build_call(
                alloc_fn,
                &[alloc_bytes.into(), align.into()],
                "vec_clone_alloc",
            )
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();
        // memcpy
        let ptr_ty = self.ctx.ptr_type(inkwell::AddressSpace::default());
        let memcpy_ty = self.ctx.void_type().fn_type(
            &[
                ptr_ty.into(),
                ptr_ty.into(),
                i64_ty.into(),
                self.ctx.bool_type().into(),
            ],
            false,
        );
        let memcpy_fn = self
            .module
            .get_function("llvm.memcpy.p0.p0.i64")
            .unwrap_or_else(|| {
                self.module
                    .add_function("llvm.memcpy.p0.p0.i64", memcpy_ty, None)
            });
        self.builder
            .build_call(
                memcpy_fn,
                &[
                    new_buf.into(),
                    buf.into(),
                    copy_bytes.into(),
                    self.ctx.bool_type().const_zero().into(),
                ],
                "",
            )
            .unwrap();
        self.vec_pack(new_buf, len, cap)
    }

    /// `@vec(a, b, c)` — alloc n, store each, len=cap=n.
    fn translate_vec_literal(&mut self, args: &[CfgValue], vec_ty: Type) -> BasicValueEnum<'ctx> {
        let n = args.len() as u64;
        let i64_ty = self.ctx.i64_type();
        let n_val = i64_ty.const_int(n, false);
        let elem_size = self.vec_elem_size(vec_ty);
        let bytes = self
            .builder
            .build_int_mul(n_val, elem_size, "lit_bytes")
            .unwrap();
        let align = i64_ty.const_int(8, false);
        let alloc_fn = self.vec_alloc_fn();
        let buf = self
            .builder
            .build_call(alloc_fn, &[bytes.into(), align.into()], "vec_lit_alloc")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();
        let elem_ty = self.vec_element_type(vec_ty);
        let elem_llvm = match gruel_type_to_llvm(elem_ty, self.ctx, self.type_pool) {
            Some(t) => t,
            None => return self.vec_pack(buf, n_val, n_val),
        };
        for (i, &arg) in args.iter().enumerate() {
            let v = self.get_value(arg);
            let idx = i64_ty.const_int(i as u64, false);
            let slot = unsafe {
                self.builder
                    .build_in_bounds_gep(elem_llvm, buf, &[idx], "lit_slot")
                    .unwrap()
            };
            self.builder.build_store(slot, v).unwrap();
        }
        self.vec_pack(buf, n_val, n_val)
    }

    /// `@vec_repeat(v, n)` — alloc n, store v in each slot.
    fn translate_vec_repeat(&mut self, args: &[CfgValue], vec_ty: Type) -> BasicValueEnum<'ctx> {
        let v = self.get_value(args[0]);
        let n_raw = self.get_value(args[1]).into_int_value();
        let n = self.zext_to_i64(n_raw);
        let i64_ty = self.ctx.i64_type();
        let elem_size = self.vec_elem_size(vec_ty);
        let bytes = self
            .builder
            .build_int_mul(n, elem_size, "rep_bytes")
            .unwrap();
        let align = i64_ty.const_int(8, false);
        let alloc_fn = self.vec_alloc_fn();
        let buf = self
            .builder
            .build_call(alloc_fn, &[bytes.into(), align.into()], "vec_rep_alloc")
            .unwrap()
            .try_as_basic_value()
            .basic()
            .unwrap()
            .into_pointer_value();
        let elem_ty = self.vec_element_type(vec_ty);
        let elem_llvm = match gruel_type_to_llvm(elem_ty, self.ctx, self.type_pool) {
            Some(t) => t,
            None => return self.vec_pack(buf, n, n),
        };
        // Loop: for i in 0..n { buf[i] = v; }
        let zero = i64_ty.const_zero();
        let one = i64_ty.const_int(1, false);
        let header_bb = self.ctx.append_basic_block(self.fn_value, "rep_header");
        let body_bb = self.ctx.append_basic_block(self.fn_value, "rep_body");
        let after_bb = self.ctx.append_basic_block(self.fn_value, "rep_after");
        let entry_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(header_bb).unwrap();
        self.builder.position_at_end(header_bb);
        let i_phi = self.builder.build_phi(i64_ty, "i").unwrap();
        i_phi.add_incoming(&[(&zero, entry_bb)]);
        let i = i_phi.as_basic_value().into_int_value();
        let cond = self
            .builder
            .build_int_compare(inkwell::IntPredicate::ULT, i, n, "cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, body_bb, after_bb)
            .unwrap();
        self.builder.position_at_end(body_bb);
        let slot = unsafe {
            self.builder
                .build_in_bounds_gep(elem_llvm, buf, &[i], "rep_slot")
                .unwrap()
        };
        self.builder.build_store(slot, v).unwrap();
        let next = self.builder.build_int_add(i, one, "i_next").unwrap();
        let body_end = self.builder.get_insert_block().unwrap();
        i_phi.add_incoming(&[(&next, body_end)]);
        self.builder.build_unconditional_branch(header_bb).unwrap();
        self.builder.position_at_end(after_bb);
        self.vec_pack(buf, n, n)
    }

    /// Drop a Vec(T) value: drop each live element if T needs drop, then
    /// free the buffer if cap > 0.
    fn translate_vec_drop(&mut self, val: BasicValueEnum<'ctx>, vec_ty: Type) {
        let recv = val.into_struct_value();
        let buf = self
            .builder
            .build_extract_value(recv, 0, "drop_buf")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_extract_value(recv, 1, "drop_len")
            .unwrap()
            .into_int_value();
        let cap = self
            .builder
            .build_extract_value(recv, 2, "drop_cap")
            .unwrap()
            .into_int_value();
        let i64_ty = self.ctx.i64_type();
        let zero = i64_ty.const_zero();
        let alive = self
            .builder
            .build_int_compare(inkwell::IntPredicate::UGT, cap, zero, "drop_alive")
            .unwrap();
        let alive_bb = self.ctx.append_basic_block(self.fn_value, "vec_drop_alive");
        let after_bb = self.ctx.append_basic_block(self.fn_value, "vec_drop_after");
        self.builder
            .build_conditional_branch(alive, alive_bb, after_bb)
            .unwrap();
        self.builder.position_at_end(alive_bb);

        // Per-element drop loop if T needs drop.
        let elem_ty = self.vec_element_type(vec_ty);
        if drop_names::type_needs_drop(elem_ty, self.type_pool) {
            self.emit_vec_drop_loop(buf, len, elem_ty);
        }

        // Free the buffer.
        let elem_size = self.vec_elem_size(vec_ty);
        let bytes = self
            .builder
            .build_int_mul(cap, elem_size, "drop_bytes")
            .unwrap();
        let align = i64_ty.const_int(8, false);
        let free_fn = self.vec_free_fn();
        self.builder
            .build_call(free_fn, &[buf.into(), bytes.into(), align.into()], "")
            .unwrap();
        self.builder.build_unconditional_branch(after_bb).unwrap();
        self.builder.position_at_end(after_bb);
    }

    /// `v.dispose()` — ADR-0067: panic if `len != 0`, otherwise free the buffer.
    ///
    /// Receiver is passed by-value (consumed). The buffer is freed even when
    /// `cap == 0` is harmless because `__gruel_free` is a no-op on
    /// zero-byte allocations / null pointers.
    fn translate_vec_dispose(&mut self, args: &[CfgValue]) {
        let recv_val = self.get_value(args[0]);
        let recv = recv_val.into_struct_value();
        let buf = self
            .builder
            .build_extract_value(recv, 0, "dispose_buf")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_extract_value(recv, 1, "dispose_len")
            .unwrap()
            .into_int_value();
        let cap = self
            .builder
            .build_extract_value(recv, 2, "dispose_cap")
            .unwrap()
            .into_int_value();
        let i64_ty = self.ctx.i64_type();
        let zero = i64_ty.const_zero();

        // panic if len != 0
        let nonempty = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, len, zero, "dispose_nonempty")
            .unwrap();
        let panic_bb = self
            .ctx
            .append_basic_block(self.fn_value, "vec_dispose_panic");
        let cont_bb = self
            .ctx
            .append_basic_block(self.fn_value, "vec_dispose_cont");
        self.builder
            .build_conditional_branch(nonempty, panic_bb, cont_bb)
            .unwrap();

        self.builder.position_at_end(panic_bb);
        let panic_fn_ty = self.ctx.void_type().fn_type(&[], false);
        let panic_fn = self
            .module
            .get_function("__gruel_vec_dispose_panic")
            .unwrap_or_else(|| {
                self.module
                    .add_function("__gruel_vec_dispose_panic", panic_fn_ty, None)
            });
        self.builder.build_call(panic_fn, &[], "").unwrap();
        self.builder.build_unreachable().unwrap();

        self.builder.position_at_end(cont_bb);
        // Free the buffer (only if cap > 0).
        let alive = self
            .builder
            .build_int_compare(inkwell::IntPredicate::UGT, cap, zero, "dispose_alive")
            .unwrap();
        let free_bb = self
            .ctx
            .append_basic_block(self.fn_value, "vec_dispose_free");
        let after_bb = self
            .ctx
            .append_basic_block(self.fn_value, "vec_dispose_after");
        self.builder
            .build_conditional_branch(alive, free_bb, after_bb)
            .unwrap();
        self.builder.position_at_end(free_bb);

        let recv_vec_ty = self.cfg.get_inst(args[0]).ty;
        let elem_size = self.vec_elem_size(recv_vec_ty);
        let bytes = self
            .builder
            .build_int_mul(cap, elem_size, "dispose_bytes")
            .unwrap();
        let align = i64_ty.const_int(8, false);
        let free_fn = self.vec_free_fn();
        self.builder
            .build_call(free_fn, &[buf.into(), bytes.into(), align.into()], "")
            .unwrap();
        self.builder.build_unconditional_branch(after_bb).unwrap();
        self.builder.position_at_end(after_bb);
    }

    /// Emit a loop that drops each element `buf[i]` for `i in 0..len`.
    fn emit_vec_drop_loop(
        &mut self,
        buf: inkwell::values::PointerValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
        elem_ty: Type,
    ) {
        let i64_ty = self.ctx.i64_type();
        let zero = i64_ty.const_zero();
        let one = i64_ty.const_int(1, false);
        let header_bb = self
            .ctx
            .append_basic_block(self.fn_value, "vec_drop_header");
        let body_bb = self.ctx.append_basic_block(self.fn_value, "vec_drop_body");
        let exit_bb = self.ctx.append_basic_block(self.fn_value, "vec_drop_exit");
        let entry_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(header_bb).unwrap();
        self.builder.position_at_end(header_bb);
        let i_phi = self.builder.build_phi(i64_ty, "i").unwrap();
        i_phi.add_incoming(&[(&zero, entry_bb)]);
        let i = i_phi.as_basic_value().into_int_value();
        let cond = self
            .builder
            .build_int_compare(inkwell::IntPredicate::ULT, i, len, "cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, body_bb, exit_bb)
            .unwrap();
        self.builder.position_at_end(body_bb);
        if let Some(elem_llvm) = gruel_type_to_llvm(elem_ty, self.ctx, self.type_pool) {
            let slot = unsafe {
                self.builder
                    .build_in_bounds_gep(elem_llvm, buf, &[i], "drop_slot")
                    .unwrap()
            };
            let elem_val = self
                .builder
                .build_load(elem_llvm, slot, "drop_elem")
                .unwrap();
            self.emit_element_drop(elem_val, elem_ty);
        }
        let next = self.builder.build_int_add(i, one, "i_next").unwrap();
        let body_end = self.builder.get_insert_block().unwrap();
        i_phi.add_incoming(&[(&next, body_end)]);
        self.builder.build_unconditional_branch(header_bb).unwrap();
        self.builder.position_at_end(exit_bb);
    }

    /// Emit a drop call for a single element value.
    fn emit_element_drop(&mut self, val: BasicValueEnum<'ctx>, elem_ty: Type) {
        if self.is_builtin_string(elem_ty) {
            let (ptr, len, cap) = self.extract_str_ptr_len_cap(val);
            let drop_fn = self.get_or_declare_drop_string();
            self.builder
                .build_call(drop_fn, &[ptr.into(), len.into(), cap.into()], "")
                .unwrap();
            return;
        }
        if matches!(elem_ty.kind(), TypeKind::Vec(_)) {
            self.translate_vec_drop(val, elem_ty);
            return;
        }
        if let Some(fn_name) = drop_names::drop_fn_name(elem_ty, self.type_pool) {
            let args: Vec<BasicValueEnum<'ctx>> = match elem_ty.kind() {
                TypeKind::Array(_) => self.extract_fields_for_drop(val, elem_ty),
                _ => vec![val],
            };
            let param_types: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> =
                args.iter().map(|v| v.get_type().into()).collect();
            let meta_args: Vec<BasicMetadataValueEnum<'ctx>> =
                args.iter().map(|v| (*v).into()).collect();
            let fn_in_module = self.module.get_function(&fn_name);
            let callee = fn_in_module.unwrap_or_else(|| {
                let fn_ty = self.ctx.void_type().fn_type(&param_types, false);
                self.module.add_function(&fn_name, fn_ty, None)
            });
            self.builder.build_call(callee, &meta_args, "").unwrap();
        }
    }

    /// `@parts_to_vec(p, len, cap)` — pack into a Vec aggregate.
    fn translate_parts_to_vec(&mut self, args: &[CfgValue], _vec_ty: Type) -> BasicValueEnum<'ctx> {
        let p = self.get_value(args[0]).into_pointer_value();
        let len_raw = self.get_value(args[1]).into_int_value();
        let cap_raw = self.get_value(args[2]).into_int_value();
        let len = self.zext_to_i64(len_raw);
        let cap = self.zext_to_i64(cap_raw);
        self.vec_pack(p, len, cap)
    }

    /// Translate a CFG terminator into LLVM control flow.
    fn translate_terminator(&mut self, term: Terminator) -> CompileResult<()> {
        match term {
            Terminator::Return { value: Some(v) } => {
                let ty = self.cfg.get_inst(v).ty;
                if gruel_type_to_llvm(ty, self.ctx, self.type_pool).is_none() {
                    self.emit_lifetime_ends();
                    if self.cfg.fn_name() == "main" {
                        let exit_fn = self.get_or_declare_exit_fn();
                        let zero = self.ctx.i32_type().const_zero();
                        self.builder
                            .build_call(exit_fn, &[zero.into()], "")
                            .unwrap();
                        self.builder.build_unreachable().unwrap();
                    } else {
                        self.builder.build_return(None).unwrap();
                    }
                } else {
                    let ret_val = self.get_value(v);
                    self.emit_lifetime_ends();
                    self.builder.build_return(Some(&ret_val)).unwrap();
                }
            }
            Terminator::Return { value: None } => {
                self.emit_lifetime_ends();
                if self.cfg.fn_name() == "main" {
                    let exit_fn = self.get_or_declare_exit_fn();
                    let zero = self.ctx.i32_type().const_zero();
                    self.builder
                        .build_call(exit_fn, &[zero.into()], "")
                        .unwrap();
                    self.builder.build_unreachable().unwrap();
                } else {
                    self.builder.build_return(None).unwrap();
                }
            }
            Terminator::Goto {
                target,
                args_start,
                args_len,
            } => {
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
                then_block,
                then_args_start,
                then_args_len,
                else_block,
                else_args_start,
                else_args_len,
            } => {
                let current_bb = self.builder.get_insert_block().unwrap();
                let cond_val = self.get_value(cond).into_int_value();
                let cond_i1 = if cond_val.get_type().get_bit_width() == 1 {
                    cond_val
                } else {
                    let zero = cond_val.get_type().const_zero();
                    self.builder
                        .build_int_compare(IntPredicate::NE, cond_val, zero, "cond")
                        .unwrap()
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
                self.builder
                    .build_conditional_branch(cond_i1, then_bb, else_bb)
                    .unwrap();
            }
            Terminator::Switch {
                scrutinee,
                cases_start,
                cases_len,
                default,
            } => {
                let raw_val = self.get_value(scrutinee);
                // Consult the type's DiscriminantStrategy (ADR-0069). For the
                // current `Separate` strategy on data enums, the discriminant
                // lives in struct field 0; for unit-only enums the value
                // itself is the discriminant.
                let val = {
                    let scrutinee_ty = self.cfg.get_inst(scrutinee).ty;
                    if let TypeKind::Enum(id) = scrutinee_ty.kind() {
                        let enum_def = self.type_pool.enum_def(id);
                        let layout = layout_of(self.type_pool, scrutinee_ty);
                        let strategy = layout
                            .discriminant_strategy()
                            .expect("enum type must have a discriminant strategy");
                        match strategy {
                            DiscriminantStrategy::Separate { .. } => {
                                if enum_def.has_data_variants() {
                                    let struct_val = raw_val.into_struct_value();
                                    self.builder
                                        .build_extract_value(struct_val, 0, "discrim")
                                        .expect("extract_value failed")
                                        .into_int_value()
                                } else {
                                    raw_val.into_int_value()
                                }
                            }
                            DiscriminantStrategy::Niche {
                                unit_variant,
                                data_variant,
                                niche_offset,
                                niche_width,
                                niche_value,
                            } => self.synthesize_niche_discriminant(
                                raw_val,
                                layout.size,
                                niche_offset,
                                niche_width,
                                niche_value,
                                unit_variant,
                                data_variant,
                                enum_def.discriminant_type(),
                            ),
                        }
                    } else {
                        raw_val.into_int_value()
                    }
                };
                let default_bb = self.llvm_block(default);
                let cases = self.cfg.get_switch_cases(cases_start, cases_len);
                // Deduplicate case values: LLVM forbids duplicate case values.
                // Keep only the first occurrence (same behavior as native backend).
                let mut seen = rustc_hash::FxHashSet::default();
                let llvm_cases: Vec<_> = cases
                    .iter()
                    .filter(|(case_val, _)| seen.insert(*case_val))
                    .map(|(case_val, case_block)| {
                        let case_int = val.get_type().const_int(*case_val as u64, true);
                        (case_int, self.llvm_block(*case_block))
                    })
                    .collect();
                self.builder
                    .build_switch(val, default_bb, &llvm_cases)
                    .unwrap();
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
    mod_ctx: &ModuleCtx<'ctx, '_>,
) -> CompileResult<()> {
    let mut fn_gen = FnCodegen::new(cfg, *fn_value, mod_ctx);
    fn_gen.translate()
}

/// LLVM integer type for a niche of `width` bytes (ADR-0069).
fn niche_int_type<'ctx>(
    ctx: &'ctx inkwell::context::Context,
    width: u8,
) -> inkwell::types::IntType<'ctx> {
    match width {
        1 => ctx.i8_type(),
        2 => ctx.i16_type(),
        4 => ctx.i32_type(),
        8 => ctx.i64_type(),
        16 => ctx.i128_type(),
        other => panic!("unsupported niche width: {other}"),
    }
}

/// Returns true for signed integer types.
fn is_signed_type(ty: Type) -> bool {
    matches!(
        ty.kind(),
        TypeKind::I8 | TypeKind::I16 | TypeKind::I32 | TypeKind::I64 | TypeKind::Isize
    )
}

/// Returns the bit width of an LLVM float type.
fn float_bit_width<'ctx>(
    float_ty: inkwell::types::FloatType<'ctx>,
    ctx: &'ctx inkwell::context::Context,
) -> u32 {
    if float_ty == ctx.f16_type() {
        16
    } else if float_ty == ctx.f32_type() {
        32
    } else {
        64
    }
}

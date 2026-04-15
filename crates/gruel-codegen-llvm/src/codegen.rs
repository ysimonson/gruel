//! LLVM IR generation from Gruel CFG.
//!
//! This module is only compiled when the `llvm18` feature is enabled.

use gruel_air::{Type, TypeInternPool};
use gruel_cfg::{Cfg, CfgInstData};
use gruel_error::{CompileError, CompileResult, ErrorKind};
use inkwell::OptimizationLevel;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target as LlvmTarget, TargetMachine,
};
use inkwell::types::BasicTypeEnum;
use inkwell::values::FunctionValue;
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

    // Define each function body.
    for (cfg, fn_value) in &declared {
        define_function(cfg, fn_value, &context, &builder, type_pool, strings, interner)?;
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

    // Collect LLVM parameter types from CFG's parameter slots.
    // Each CFG param slot corresponds to one Gruel parameter (or one flattened
    // struct field slot). The type for slot i is found by scanning the entry
    // block for `Param { index: i }` instructions.
    let param_types = collect_param_types(cfg, ctx, type_pool);

    // Build the LLVM function type.
    let fn_type = match gruel_type_to_llvm(cfg.return_type(), ctx, type_pool) {
        Some(ret_ty) => ret_ty.fn_type(&param_types, false),
        None => {
            // Unit or Never return type → LLVM void function.
            ctx.void_type().fn_type(&param_types, false)
        }
    };

    Ok(module.add_function(name, fn_type, None))
}

/// Collect LLVM parameter types for a function's parameter slots.
///
/// Scans all `Param { index }` instructions in the entry block to find the
/// type of each slot. Slots with no matching `Param` instruction (e.g.,
/// write-only inout params that are never read) default to `i8*` / `ptr`.
fn collect_param_types<'ctx>(
    cfg: &Cfg,
    ctx: &'ctx Context,
    type_pool: &TypeInternPool,
) -> Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> {
    let num_params = cfg.num_params() as usize;
    if num_params == 0 {
        return vec![];
    }

    // Build a slot → type map from Param instructions in all blocks.
    let mut slot_types: Vec<Option<Type>> = vec![None; num_params];
    for block in cfg.blocks() {
        for &val in &block.insts {
            let inst = cfg.get_inst(val);
            if let CfgInstData::Param { index } = inst.data {
                let slot = index as usize;
                if slot < num_params && slot_types[slot].is_none() {
                    slot_types[slot] = Some(inst.ty);
                }
            }
        }
    }

    // Resolve each slot to an LLVM type.
    // Inout params are passed as opaque pointers; scalar params use their type.
    slot_types
        .into_iter()
        .enumerate()
        .filter_map(|(i, ty_opt)| {
            if cfg.is_param_inout(i as u32) {
                // Inout → opaque pointer.
                Some(
                    ctx.ptr_type(inkwell::AddressSpace::default())
                        .into(),
                )
            } else {
                let ty = ty_opt?;
                gruel_type_to_llvm_param(ty, ctx, type_pool)
            }
        })
        .collect()
}

/// Generate the body of a declared LLVM function from its CFG.
///
/// Phase 2a scaffold: each CFG block becomes an LLVM basic block containing
/// an `unreachable` terminator. The actual instruction translation is Phase 2b.
fn define_function<'ctx>(
    cfg: &Cfg,
    fn_value: &FunctionValue<'ctx>,
    ctx: &'ctx Context,
    builder: &inkwell::builder::Builder<'ctx>,
    type_pool: &TypeInternPool,
    _strings: &[String],
    _interner: &ThreadedRodeo,
) -> CompileResult<()> {
    // Create one LLVM basic block per CFG block.
    let llvm_blocks: Vec<inkwell::basic_block::BasicBlock<'ctx>> = cfg
        .blocks()
        .iter()
        .map(|bb| ctx.append_basic_block(*fn_value, &format!("bb{}", bb.id.0)))
        .collect();

    // Stub each block with `unreachable` — Phase 2b fills in the real translation.
    for llvm_bb in &llvm_blocks {
        builder.position_at_end(*llvm_bb);
        builder.build_unreachable()
            .map_err(|e| llvm_error(format!("build_unreachable failed: {}", e)))?;
    }

    Ok(())
}

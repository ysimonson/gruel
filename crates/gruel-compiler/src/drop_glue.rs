//! Drop glue synthesis for structs and arrays with non-trivial fields/elements.
//!
//! When a struct has fields that need drop (like String), the compiler needs to
//! generate a "drop glue" function that drops each field. This is similar to
//! Rust's drop glue.
//!
//! For example, for a struct like:
//! ```text
//! struct Container {
//!     name: String,
//!     value: i32,
//! }
//! ```
//!
//! We generate a function `__gruel_drop_Container` that:
//! 1. Receives the whole struct as a single parameter (matching the user destructor ABI)
//! 2. Calls the user-defined destructor first, if any (`Container.__drop`)
//! 3. Drops each field that needs dropping via `FieldGet` (in declaration order)
//!
//! For arrays like `[String; 3]`, we generate a function `__gruel_drop_array_String_3` that:
//! 1. Receives all element slots as parameters (one LLVM param per element)
//! 2. Drops each element in index order (element 0 first, then 1, etc.)

use gruel_air::{
    Air, AirArgMode, AirInst, AirInstData, AirParamMode, AirPattern, AirRef, AnalyzedFunction,
    StructDef, Type, TypeInternPool, TypeKind,
};
use gruel_cfg::drop_names;
use gruel_util::Span;
use lasso::ThreadedRodeo;

/// Check if a type needs drop.
fn type_needs_drop(ty: Type, type_pool: &TypeInternPool) -> bool {
    drop_names::type_needs_drop(ty, type_pool)
}

/// If `statements` is non-empty, wrap them in a Block whose value is `value` and
/// return the resulting AirRef. If empty, return `value` unchanged. The block's
/// type is taken to be Type::UNIT — every call site here produces unit.
fn block_or_value(air: &mut Air, statements: &[AirRef], value: AirRef, span: Span) -> AirRef {
    if statements.is_empty() {
        return value;
    }
    let stmt_u32s: Vec<u32> = statements.iter().map(|r| r.as_u32()).collect();
    let stmts_start = air.add_extra(&stmt_u32s);
    let stmts_len = statements.len() as u32;
    air.add_inst(AirInst {
        data: AirInstData::Block {
            stmts_start,
            stmts_len,
            value,
        },
        ty: Type::UNIT,
        span,
    })
}

fn unit_const(air: &mut Air, span: Span) -> AirRef {
    air.add_inst(AirInst {
        data: AirInstData::UnitConst,
        ty: Type::UNIT,
        span,
    })
}

/// Synthesize drop glue functions for all structs and arrays that need them.
///
/// Returns a list of synthesized functions that should be added to the compilation.
pub fn synthesize_drop_glue(
    type_pool: &TypeInternPool,
    interner: &ThreadedRodeo,
) -> Vec<AnalyzedFunction> {
    let mut drop_glue_functions = Vec::new();

    // Create drop glue for structs
    for struct_id in type_pool.all_struct_ids() {
        let struct_def = type_pool.struct_def(struct_id);
        // Skip structs that don't need drop
        let struct_ty = Type::new_struct(struct_id);
        if !type_needs_drop(struct_ty, type_pool) {
            continue;
        }

        // Skip builtins that have runtime-provided destructors (e.g., String)
        // to avoid duplicate symbol errors. User-defined destructors still need
        // synthesized drop glue.
        if struct_def.is_builtin && struct_def.destructor.is_some() {
            continue;
        }

        // Create drop glue function for struct
        let func = create_struct_drop_glue_function(&struct_def, struct_id, type_pool, interner);
        drop_glue_functions.push(func);
    }

    // Create drop glue for data enums with droppable fields and/or user destructors
    for enum_id in type_pool.all_enum_ids() {
        let enum_ty = Type::new_enum(enum_id);
        if !type_needs_drop(enum_ty, type_pool) {
            continue;
        }
        let func = create_enum_drop_glue_function(enum_id, type_pool, interner);
        drop_glue_functions.push(func);
    }

    // Create drop glue for arrays
    for array_id in type_pool.all_array_ids() {
        // Skip arrays that don't need drop
        let array_ty = Type::new_array(array_id);
        if !type_needs_drop(array_ty, type_pool) {
            continue;
        }

        // Create drop glue function for array
        let func = create_array_drop_glue_function(array_id, type_pool);
        drop_glue_functions.push(func);
    }

    drop_glue_functions
}

/// Create a drop glue function for a single struct.
///
/// The synthesized function takes the **whole struct** as its single parameter
/// (same convention as a user-defined `drop fn`), not individual flattened fields.
/// This allows calling the user destructor directly and using `FieldGet` to
/// extract individual fields for recursive drops.
fn create_struct_drop_glue_function(
    struct_def: &StructDef,
    struct_id: gruel_air::StructId,
    type_pool: &TypeInternPool,
    interner: &ThreadedRodeo,
) -> AnalyzedFunction {
    let fn_name = format!("__gruel_drop_{}", struct_def.name);
    let span = Span::new(0, 0); // Synthetic span

    let struct_ty = Type::new_struct(struct_id);
    // num_param_slots = abi slot count of the struct (sum of field slot counts).
    let num_param_slots = type_pool.abi_slot_count(struct_ty);

    let mut air = Air::new(Type::UNIT);

    // Single parameter: the whole struct value.
    let param_ref = air.add_inst(AirInst {
        data: AirInstData::Param { index: 0 },
        ty: struct_ty,
        span,
    });

    let mut drop_statements = Vec::new();

    // Call user destructor first (before dropping fields).
    // Builtins with runtime destructors (e.g. String) are excluded by the caller.
    if let Some(destructor_name) = &struct_def.destructor {
        let name_spur = interner.get_or_intern(destructor_name.as_str());
        // Pass the whole struct as a single arg (matches user destructor ABI).
        let args_u32s = [param_ref.as_u32(), AirArgMode::Normal.as_u32()];
        let args_start = air.add_extra(&args_u32s);
        let call_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: name_spur,
                args_start,
                args_len: 1,
            },
            ty: Type::UNIT,
            span,
        });
        drop_statements.push(call_ref);
    }

    // Then drop any fields that need it (in declaration order).
    // Use FieldGet to extract each field from the struct parameter.
    for (field_idx, field) in struct_def.fields.iter().enumerate() {
        if type_needs_drop(field.ty, type_pool) {
            let field_val = air.add_inst(AirInst {
                data: AirInstData::FieldGet {
                    base: param_ref,
                    struct_id,
                    field_index: field_idx as u32,
                },
                ty: field.ty,
                span,
            });
            let drop_ref = air.add_inst(AirInst {
                data: AirInstData::Drop { value: field_val },
                ty: Type::UNIT,
                span,
            });
            drop_statements.push(drop_ref);
        }
    }

    // Wrap side-effect statements in a Block so they are executed.
    // The CFG builder uses demand-driven lowering, so statements must be
    // explicitly listed as block side-effects.
    let unit = unit_const(&mut air, span);
    let return_value = block_or_value(&mut air, &drop_statements, unit, span);

    air.add_inst(AirInst {
        data: AirInstData::Ret(Some(return_value)),
        ty: Type::UNIT,
        span,
    });

    // param_slot_types: the struct type repeated num_param_slots times.
    // collect_param_types sees type=struct_ty at slot 0, advances by num_param_slots,
    // and emits exactly one LLVM param of the struct's aggregate type.
    let param_modes = vec![AirParamMode::Normal; num_param_slots as usize];
    let param_slot_types = vec![struct_ty; num_param_slots as usize];

    AnalyzedFunction {
        name: fn_name,
        air,
        num_locals: 0,
        num_param_slots,
        param_modes,
        param_slot_types,
        is_destructor: true,
    }
}

/// Create a drop glue function for an array type.
///
/// The function receives all element slots as parameters (flattened) and drops
/// each element in index order.
fn create_array_drop_glue_function(
    array_id: gruel_air::ArrayTypeId,
    type_pool: &TypeInternPool,
) -> AnalyzedFunction {
    let array_ty = Type::new_array(array_id);
    let fn_name = drop_names::drop_fn_name(array_ty, type_pool)
        .expect("array drop glue called for non-droppable array");
    let span = Span::new(0, 0); // Synthetic span

    // Get array element type and length
    let (element_type, length) = type_pool.array_def(array_id);

    // Create AIR for the drop glue function
    let mut air = Air::new(Type::UNIT);

    // Calculate total parameter slots (element slots * length)
    let element_slot_count = type_pool.abi_slot_count(element_type);
    let num_param_slots = element_slot_count * length as u32;

    // Collect drop statements for each element
    let mut drop_statements = Vec::new();

    // For each element, emit a Drop instruction.
    // Type::Struct handles both user-defined structs and builtin String.
    for elem_idx in 0..length {
        let current_param_slot = elem_idx as u32 * element_slot_count;

        // Emit Drop for this element
        match element_type.kind() {
            TypeKind::Struct(struct_id) => {
                let param_ref = air.add_inst(AirInst {
                    data: AirInstData::Param {
                        index: current_param_slot,
                    },
                    ty: Type::new_struct(struct_id),
                    span,
                });
                let drop_ref = air.add_inst(AirInst {
                    data: AirInstData::Drop { value: param_ref },
                    ty: Type::UNIT,
                    span,
                });
                drop_statements.push(drop_ref);
            }
            TypeKind::Array(nested_array_id) => {
                let param_ref = air.add_inst(AirInst {
                    data: AirInstData::Param {
                        index: current_param_slot,
                    },
                    ty: Type::new_array(nested_array_id),
                    span,
                });
                let drop_ref = air.add_inst(AirInst {
                    data: AirInstData::Drop { value: param_ref },
                    ty: Type::UNIT,
                    span,
                });
                drop_statements.push(drop_ref);
            }
            // Primitives don't need drop - this shouldn't happen since we check type_needs_drop
            _ => {}
        }
    }

    // If we have drop statements, wrap them in a Block so they get executed
    let unit = unit_const(&mut air, span);
    let return_value = block_or_value(&mut air, &drop_statements, unit, span);

    // Add return instruction
    air.add_inst(AirInst {
        data: AirInstData::Ret(Some(return_value)),
        ty: Type::UNIT,
        span,
    });

    // All parameters are passed by value (normal mode)
    let param_modes = vec![AirParamMode::Normal; num_param_slots as usize];
    // Each element contributes element_slot_count slots of element_type
    let param_slot_types: Vec<Type> =
        std::iter::repeat_n(element_type, num_param_slots as usize).collect();

    AnalyzedFunction {
        name: fn_name,
        air,
        num_locals: 0,
        num_param_slots,
        param_modes,
        param_slot_types,
        is_destructor: true,
    }
}

/// Create a drop glue function for a data enum type.
///
/// The synthesized function takes the whole enum value as its single parameter
/// (`is_destructor: true` so the CFG builder doesn't auto-drop it).
/// It emits a match on the discriminant; for each variant that has droppable fields,
/// the arm body extracts and drops those fields via `EnumPayloadGet`.
fn create_enum_drop_glue_function(
    enum_id: gruel_air::EnumId,
    type_pool: &TypeInternPool,
    interner: &ThreadedRodeo,
) -> AnalyzedFunction {
    let enum_def = type_pool.enum_def(enum_id);
    let fn_name = format!("__gruel_drop_{}", enum_def.name);
    let span = Span::new(0, 0); // Synthetic span

    let enum_ty = Type::new_enum(enum_id);
    // Enums always occupy exactly one ABI slot.
    let num_param_slots = 1u32;

    let mut air = Air::new(Type::UNIT);

    // Single parameter: the whole enum value.
    let param_ref = air.add_inst(AirInst {
        data: AirInstData::Param { index: 0 },
        ty: enum_ty,
        span,
    });

    // Pre-match statements: call user destructor first, if any (ADR-0053 phase 3b).
    // The user destructor takes `self` by value and returns unit. After it runs,
    // we dispatch on the discriminant and drop the owning fields of the active
    // variant. This mirrors struct semantics from ADR-0010.
    let mut pre_match_stmts: Vec<AirRef> = Vec::new();
    if let Some(destructor_name) = &enum_def.destructor {
        let name_spur = interner.get_or_intern(destructor_name.as_str());
        let args_u32s = [param_ref.as_u32(), AirArgMode::Normal.as_u32()];
        let args_start = air.add_extra(&args_u32s);
        let call_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: name_spur,
                args_start,
                args_len: 1,
            },
            ty: Type::UNIT,
            span,
        });
        pre_match_stmts.push(call_ref);
    }

    // Build one match arm per variant.
    let mut arms_data: Vec<u32> = Vec::new();
    let arms_len = enum_def.variants.len() as u32;

    for (variant_idx, variant_def) in enum_def.variants.iter().enumerate() {
        // Collect Drop instructions for droppable fields in this variant.
        let mut drop_stmts: Vec<AirRef> = Vec::new();

        for (field_idx, &field_ty) in variant_def.fields.iter().enumerate() {
            if type_needs_drop(field_ty, type_pool) {
                let field_val = air.add_inst(AirInst {
                    data: AirInstData::EnumPayloadGet {
                        base: param_ref,
                        variant_index: variant_idx as u32,
                        field_index: field_idx as u32,
                    },
                    ty: field_ty,
                    span,
                });
                let drop_ref = air.add_inst(AirInst {
                    data: AirInstData::Drop { value: field_val },
                    ty: Type::UNIT,
                    span,
                });
                drop_stmts.push(drop_ref);
            }
        }

        // Arm body: block with drop statements, or just unit if nothing to drop.
        let unit = unit_const(&mut air, span);
        let body: AirRef = block_or_value(&mut air, &drop_stmts, unit, span);

        AirPattern::EnumVariant {
            enum_id,
            variant_index: variant_idx as u32,
        }
        .encode(body, &mut arms_data);
    }

    // Emit the match on the enum parameter.
    let arms_start = air.add_extra(&arms_data);
    let match_result = air.add_inst(AirInst {
        data: AirInstData::Match {
            scrutinee: param_ref,
            arms_start,
            arms_len,
        },
        ty: Type::UNIT,
        span,
    });

    // If we have a user destructor, wrap the pre-match call(s) + match in a Block
    // so the user destructor executes before the variant-dispatch drops.
    let return_value = if pre_match_stmts.is_empty() {
        match_result
    } else {
        pre_match_stmts.push(match_result);
        let unit = unit_const(&mut air, span);
        block_or_value(&mut air, &pre_match_stmts, unit, span)
    };

    // Return unit.
    air.add_inst(AirInst {
        data: AirInstData::Ret(Some(return_value)),
        ty: Type::UNIT,
        span,
    });

    let param_modes = vec![AirParamMode::Normal; num_param_slots as usize];
    let param_slot_types = vec![enum_ty; num_param_slots as usize];

    AnalyzedFunction {
        name: fn_name,
        air,
        num_locals: 0,
        num_param_slots,
        param_modes,
        param_slot_types,
        is_destructor: true,
    }
}

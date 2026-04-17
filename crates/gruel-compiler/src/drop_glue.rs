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
    Air, AirArgMode, AirInst, AirInstData, AnalyzedFunction, StructDef, Type, TypeInternPool,
    TypeKind,
};
use gruel_cfg::drop_names;
use gruel_span::Span;
use lasso::ThreadedRodeo;

/// Check if a type needs drop.
fn type_needs_drop(ty: Type, type_pool: &TypeInternPool) -> bool {
    drop_names::type_needs_drop(ty, type_pool)
}

/// Count the number of ABI slots a type uses (flattened).
fn type_slot_count(ty: Type, type_pool: &TypeInternPool) -> u32 {
    match ty.kind() {
        // Primitives use 1 slot
        // ComptimeType uses 0 slots (comptime-only, no runtime representation)
        TypeKind::I8
        | TypeKind::I16
        | TypeKind::I32
        | TypeKind::I64
        | TypeKind::U8
        | TypeKind::U16
        | TypeKind::U32
        | TypeKind::U64
        | TypeKind::Bool
        | TypeKind::Unit
        | TypeKind::Never
        | TypeKind::Error
        | TypeKind::Enum(_) => 1,
        TypeKind::ComptimeType => 0,

        // Struct uses sum of all field slots (including builtin String with 3 fields)
        TypeKind::Struct(struct_id) => {
            let struct_def = type_pool.struct_def(struct_id);
            struct_def
                .fields
                .iter()
                .map(|f| type_slot_count(f.ty, type_pool))
                .sum()
        }

        // Note: String is now Type::Struct with is_builtin=true, handled above

        // Array uses element slots * length
        TypeKind::Array(array_id) => {
            let (element_type, length) = type_pool.array_def(array_id);
            type_slot_count(element_type, type_pool) * length as u32
        }

        // Pointer types use 1 slot (they're 64-bit addresses)
        TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => 1,

        // Module types don't take ABI slots (compile-time only)
        TypeKind::Module(_) => 0,
    }
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
    let num_param_slots = type_slot_count(struct_ty, type_pool);

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

    // Create the unit value for return
    let unit_const = air.add_inst(AirInst {
        data: AirInstData::UnitConst,
        ty: Type::UNIT,
        span,
    });

    // Wrap side-effect statements in a Block so they are executed.
    // The CFG builder uses demand-driven lowering, so statements must be
    // explicitly listed as block side-effects.
    let return_value = if drop_statements.is_empty() {
        unit_const
    } else {
        let stmt_u32s: Vec<u32> = drop_statements.iter().map(|r| r.as_u32()).collect();
        let stmts_start = air.add_extra(&stmt_u32s);
        let stmts_len = drop_statements.len() as u32;
        air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start,
                stmts_len,
                value: unit_const,
            },
            ty: Type::UNIT,
            span,
        })
    };

    air.add_inst(AirInst {
        data: AirInstData::Ret(Some(return_value)),
        ty: Type::UNIT,
        span,
    });

    // param_slot_types: the struct type repeated num_param_slots times.
    // collect_param_types sees type=struct_ty at slot 0, advances by num_param_slots,
    // and emits exactly one LLVM param of the struct's aggregate type.
    let param_modes = vec![false; num_param_slots as usize];
    let param_slot_types = vec![struct_ty; num_param_slots as usize];

    AnalyzedFunction {
        name: fn_name,
        air,
        num_locals: 0,
        num_param_slots,
        param_modes,
        param_slot_types,
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
    let element_slot_count = type_slot_count(element_type, type_pool);
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

    // Create the unit value for return
    let unit_const = air.add_inst(AirInst {
        data: AirInstData::UnitConst,
        ty: Type::UNIT,
        span,
    });

    // If we have drop statements, wrap them in a Block so they get executed
    let return_value = if drop_statements.is_empty() {
        unit_const
    } else {
        let stmt_u32s: Vec<u32> = drop_statements.iter().map(|r| r.as_u32()).collect();
        let stmts_start = air.add_extra(&stmt_u32s);
        let stmts_len = drop_statements.len() as u32;
        air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start,
                stmts_len,
                value: unit_const,
            },
            ty: Type::UNIT,
            span,
        })
    };

    // Add return instruction
    air.add_inst(AirInst {
        data: AirInstData::Ret(Some(return_value)),
        ty: Type::UNIT,
        span,
    });

    // All parameters are passed by value (normal mode)
    let param_modes = vec![false; num_param_slots as usize];
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
    }
}


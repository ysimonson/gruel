//! Clone glue synthesis for `@derive(Clone)` structs (ADR-0065 Phase 2).
//!
//! When a struct is marked with `@derive(Clone)`, the compiler synthesizes a
//! `<TypeName>.clone(borrow self) -> Self` method that constructs a new
//! instance with each field cloned. v1 only supports structs whose fields are
//! all `Copy` types — for those, "cloning a field" is just `FieldGet` with no
//! recursive call. Non-Copy fields require dispatching to each field's clone
//! method, which is more involved AIR synthesis; that's deferred.
//!
//! This module is structurally parallel to `drop_glue.rs`: walk every struct
//! that needs the synthesis, build an `Air` body, return an `AnalyzedFunction`
//! that flows through the standard CFG / codegen pipeline.

use gruel_air::{
    Air, AirInst, AirInstData, AirParamMode, AnalyzedFunction, StructDef, Type, TypeInternPool,
};
use gruel_util::Span;

/// Synthesize clone glue functions for every `@derive(Clone)` struct.
pub fn synthesize_clone_glue(type_pool: &TypeInternPool) -> Vec<AnalyzedFunction> {
    let mut out = Vec::new();
    for struct_id in type_pool.all_struct_ids() {
        let struct_def = type_pool.struct_def(struct_id);
        if !struct_def.is_clone {
            continue;
        }
        // Built-in types ship hand-written clone implementations via the
        // BuiltinTypeDef / runtime path; never synthesize over them.
        if struct_def.is_builtin {
            continue;
        }
        if let Some(func) = create_struct_clone_glue_function(&struct_def, struct_id, type_pool) {
            out.push(func);
        }
    }
    out
}

/// Build the AIR for `<TypeName>.clone(borrow self) -> Self`.
///
/// Body shape:
/// ```text
/// fn clone(borrow self) -> Self {
///     Self { f0: self.f0, f1: self.f1, ... }
/// }
/// ```
///
/// All fields must be Copy (validated at sema time), so each field is read by
/// `FieldGet` with no recursive clone call.
fn create_struct_clone_glue_function(
    struct_def: &StructDef,
    struct_id: gruel_air::StructId,
    _type_pool: &TypeInternPool,
) -> Option<AnalyzedFunction> {
    let fn_name = format!("{}.clone", struct_def.name);
    let span = Span::new(0, 0);
    let struct_ty = Type::new_struct(struct_id);

    let mut air = Air::new(struct_ty);

    // Param 0: `borrow self`. The receiver is the only parameter; clone takes
    // no other args. AirParamMode::Borrow → LLVM `ptr` at the ABI level (per
    // is_param_by_ref + collect_param_types in codegen).
    let param_ref = air.add_inst(AirInst {
        data: AirInstData::Param { index: 0 },
        ty: struct_ty,
        span,
    });

    // Build a FieldGet for each field in declaration order.
    let mut field_refs = Vec::with_capacity(struct_def.fields.len());
    for (idx, field) in struct_def.fields.iter().enumerate() {
        let field_ref = air.add_inst(AirInst {
            data: AirInstData::FieldGet {
                base: param_ref,
                struct_id,
                field_index: idx as u32,
            },
            ty: field.ty,
            span,
        });
        field_refs.push(field_ref);
    }

    // StructInit { struct_id, fields, source_order=identity }.
    let field_u32s: Vec<u32> = field_refs.iter().map(|r| r.as_u32()).collect();
    let fields_start = air.add_extra(&field_u32s);
    let fields_len = field_refs.len() as u32;
    let source_order: Vec<u32> = (0..fields_len).collect();
    let source_order_start = air.add_extra(&source_order);

    let result_ref = air.add_inst(AirInst {
        data: AirInstData::StructInit {
            struct_id,
            fields_start,
            fields_len,
            source_order_start,
        },
        ty: struct_ty,
        span,
    });

    air.add_inst(AirInst {
        data: AirInstData::Ret(Some(result_ref)),
        ty: struct_ty,
        span,
    });

    // Single by-ref param: 1 ABI slot (the LLVM `ptr`).
    let num_param_slots: u32 = 1;
    let param_modes = vec![AirParamMode::Borrow];
    let param_slot_types = vec![struct_ty];

    Some(AnalyzedFunction {
        name: fn_name,
        air,
        num_locals: 0,
        num_param_slots,
        param_modes,
        param_slot_types,
        is_destructor: false,
    })
}

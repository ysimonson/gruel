//! Gruel type → LLVM type mapping.
//!
//! After ADR-0069 phase 2, all size/alignment queries route through
//! `gruel_air::layout::layout_of`. This module is a thin LLVM-type adapter on
//! top of that layout query.

use gruel_air::layout::{DiscriminantStrategy, layout_of};
use gruel_air::{Type, TypeInternPool, TypeKind};
use inkwell::AddressSpace;
use inkwell::context::Context;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};

/// Convert a Gruel [`Type`] to an inkwell [`BasicTypeEnum`].
///
/// Returns `None` for types that map to LLVM `void`:
/// - `()` (unit)
/// - `!` (never)
///
/// Composite types are constructed recursively:
/// - Structs → packed `{field_types...}`
/// - Arrays → `[N x elem_type]`
/// - Pointers → opaque `ptr` (LLVM ≥ 15 opaque-pointer mode)
///
/// Returns `None` for internal / non-code-gen types (`error`, `type`, enums,
/// modules).
pub fn gruel_type_to_llvm<'ctx>(
    ty: Type,
    ctx: &'ctx Context,
    type_pool: &TypeInternPool,
) -> Option<BasicTypeEnum<'ctx>> {
    match ty.kind() {
        // Signed and unsigned integers share LLVM integer types.
        TypeKind::I8 | TypeKind::U8 => Some(ctx.i8_type().into()),
        TypeKind::I16 | TypeKind::U16 => Some(ctx.i16_type().into()),
        TypeKind::I32 | TypeKind::U32 => Some(ctx.i32_type().into()),
        TypeKind::I64 | TypeKind::U64 => Some(ctx.i64_type().into()),
        // Pointer-sized integers: i64 on all current 64-bit targets
        TypeKind::Isize | TypeKind::Usize => Some(ctx.i64_type().into()),

        // Floating-point types map to LLVM float types.
        TypeKind::F16 => Some(ctx.f16_type().into()),
        TypeKind::F32 => Some(ctx.f32_type().into()),
        TypeKind::F64 => Some(ctx.f64_type().into()),

        // Booleans are i1 in LLVM IR.
        TypeKind::Bool => Some(ctx.bool_type().into()),

        // ADR-0071: char lowers to i32 (storage holds the Unicode scalar value).
        TypeKind::Char => Some(ctx.i32_type().into()),

        // Unit and Never both map to LLVM void (no value).
        TypeKind::Unit | TypeKind::Never => None,

        TypeKind::Struct(id) => {
            let def = type_pool.struct_def(id);
            let field_types: Vec<BasicTypeEnum<'ctx>> = def
                .fields
                .iter()
                .filter_map(|f| gruel_type_to_llvm(f.ty, ctx, type_pool))
                .collect();
            if field_types.is_empty() {
                // Zero-sized struct (no non-void fields) → maps to LLVM void.
                // This covers both truly empty structs (`struct E {}`) and structs
                // whose only fields are unit-typed.
                None
            } else {
                // false = not packed (use Gruel's ABI alignment, not byte-packed)
                Some(ctx.struct_type(&field_types, false).into())
            }
        }

        TypeKind::Array(id) => {
            let (elem_ty, len) = type_pool.array_def(id);
            if len == 0 {
                // Zero-length array has no values → maps to LLVM void.
                return None;
            }
            let elem_llvm = gruel_type_to_llvm(elem_ty, ctx, type_pool)?;
            let n = len as u32;
            Some(match elem_llvm {
                BasicTypeEnum::IntType(t) => t.array_type(n).into(),
                BasicTypeEnum::FloatType(t) => t.array_type(n).into(),
                BasicTypeEnum::PointerType(t) => t.array_type(n).into(),
                BasicTypeEnum::StructType(t) => t.array_type(n).into(),
                BasicTypeEnum::ArrayType(t) => t.array_type(n).into(),
                BasicTypeEnum::VectorType(t) => t.array_type(n).into(),
                BasicTypeEnum::ScalableVectorType(_) => {
                    unreachable!("Gruel has no scalable vector types")
                }
            })
        }

        // Raw pointers → opaque `ptr` (LLVM ≥ 15).
        TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => {
            Some(ctx.ptr_type(AddressSpace::default()).into())
        }

        // References (ADR-0062) lower identically to today's borrows: a single
        // opaque pointer. Borrow-check enforces the safety properties.
        TypeKind::Ref(_) | TypeKind::MutRef(_) => {
            Some(ctx.ptr_type(AddressSpace::default()).into())
        }

        // Slices (ADR-0064): fat pointer `{ ptr, i64 }`.
        // First field is the data pointer, second is the length.
        TypeKind::Slice(_) | TypeKind::MutSlice(_) => {
            let ptr = ctx.ptr_type(AddressSpace::default()).into();
            let len = ctx.i64_type().into();
            Some(ctx.struct_type(&[ptr, len], false).into())
        }

        // Vec(T) (ADR-0066): owned fat pointer `{ ptr, i64, i64 }`.
        // (data pointer, length, capacity).
        TypeKind::Vec(_) => {
            let ptr = ctx.ptr_type(AddressSpace::default()).into();
            let i64_ty = ctx.i64_type().into();
            Some(ctx.struct_type(&[ptr, i64_ty, i64_ty], false).into())
        }

        // Enums:
        // - Unit-only enums: represented as their discriminant integer type (backward compat).
        // - Data enums (separate strategy): tagged union `{ discriminant_type, [max_payload_bytes x i8] }`.
        // - Data enums (niche strategy): `[size x i8]` byte array — no separate discriminant (ADR-0069).
        TypeKind::Enum(id) => {
            let def = type_pool.enum_def(id);
            if def.is_unit_only() {
                // C-style enum: just the discriminant integer.
                return gruel_type_to_llvm(def.discriminant_type(), ctx, type_pool);
            }
            let layout = layout_of(type_pool, ty);
            match layout.discriminant_strategy() {
                Some(DiscriminantStrategy::Niche { .. }) => {
                    // Niche-encoded: storage is exactly the payload's bytes.
                    Some(ctx.i8_type().array_type(layout.size as u32).into())
                }
                Some(DiscriminantStrategy::Separate { .. }) | None => {
                    // Data enum: tagged union struct.
                    let discrim_llvm = gruel_type_to_llvm(def.discriminant_type(), ctx, type_pool)?;
                    let max_payload: u64 = def
                        .variants
                        .iter()
                        .map(|v| {
                            v.fields
                                .iter()
                                .map(|f| layout_of(type_pool, *f).size)
                                .sum::<u64>()
                        })
                        .max()
                        .unwrap_or(0);
                    if max_payload == 0 {
                        // All variants happen to have empty payloads (shouldn't happen for
                        // has_data_variants() == true, but handle gracefully).
                        Some(discrim_llvm)
                    } else {
                        let byte_arr = ctx.i8_type().array_type(max_payload as u32);
                        Some(
                            ctx.struct_type(&[discrim_llvm, byte_arr.into()], false)
                                .into(),
                        )
                    }
                }
            }
        }

        // Non-code-gen types — not representable in LLVM IR.
        TypeKind::Error
        | TypeKind::ComptimeType
        | TypeKind::ComptimeStr
        | TypeKind::ComptimeInt
        | TypeKind::Module(_) => None,

        // Interfaces (ADR-0056): runtime fat pointer = `{ ptr, ptr }`.
        // First pointer is the type-erased data, second is a static vtable.
        TypeKind::Interface(_) => {
            let ptr = ctx.ptr_type(AddressSpace::default()).into();
            Some(ctx.struct_type(&[ptr, ptr], false).into())
        }
    }
}

/// Convert a Gruel type to an LLVM function parameter type.
///
/// This is the same as [`gruel_type_to_llvm`] but wrapped in
/// [`BasicMetadataTypeEnum`], which is what inkwell's function-type builder
/// requires for parameter lists.
pub fn gruel_type_to_llvm_param<'ctx>(
    ty: Type,
    ctx: &'ctx Context,
    type_pool: &TypeInternPool,
) -> Option<BasicMetadataTypeEnum<'ctx>> {
    gruel_type_to_llvm(ty, ctx, type_pool).map(Into::into)
}

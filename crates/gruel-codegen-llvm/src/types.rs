//! Gruel type → LLVM type mapping.

use gruel_air::{Type, TypeInternPool, TypeKind};
use inkwell::AddressSpace;
use inkwell::context::Context;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};

/// Return the ABI alignment of a Gruel type in bytes.
///
/// This mirrors LLVM's alignment rules for non-packed struct layout on a
/// 64-bit target. Used by [`type_byte_size`] to compute struct sizes with
/// correct inter-field and tail padding.
pub fn type_alignment(ty: Type, type_pool: &TypeInternPool) -> u64 {
    match ty.kind() {
        TypeKind::I8 | TypeKind::U8 | TypeKind::Bool => 1,
        TypeKind::I16 | TypeKind::U16 => 2,
        TypeKind::I32 | TypeKind::U32 => 4,
        TypeKind::I64 | TypeKind::U64 => 8,
        TypeKind::Isize | TypeKind::Usize => 8, // Pointer-sized: 8-byte aligned on 64-bit targets
        TypeKind::F16 => 2,
        TypeKind::F32 => 4,
        TypeKind::F64 => 8,
        TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => 8,
        TypeKind::Struct(id) => {
            let def = type_pool.struct_def(id);
            def.fields
                .iter()
                .map(|f| type_alignment(f.ty, type_pool))
                .max()
                .unwrap_or(1)
        }
        TypeKind::Array(id) => {
            let (elem_ty, _) = type_pool.array_def(id);
            type_alignment(elem_ty, type_pool)
        }
        TypeKind::Enum(id) => {
            let def = type_pool.enum_def(id);
            if def.is_unit_only() {
                type_alignment(def.discriminant_type(), type_pool)
            } else {
                // Tagged union struct { discrim, [N x i8] }:
                // [N x i8] has alignment 1, so struct alignment = discrim alignment.
                type_alignment(def.discriminant_type(), type_pool)
            }
        }
        _ => 1,
    }
}

/// Compute the ABI size of a Gruel type in bytes.
///
/// Returns the number of bytes that an LLVM `store` of this type writes.
/// For structs, this includes inter-field alignment padding and tail padding
/// (matching LLVM's non-packed struct layout). This is critical for computing
/// enum variant payload sizes — a struct field in an enum variant occupies its
/// full ABI size, not just the sum of its scalar fields.
///
/// Returns 0 for zero-sized types (unit, never, etc.).
pub fn type_byte_size(ty: Type, type_pool: &TypeInternPool) -> u64 {
    match ty.kind() {
        TypeKind::I8 | TypeKind::U8 | TypeKind::Bool => 1,
        TypeKind::I16 | TypeKind::U16 => 2,
        TypeKind::I32 | TypeKind::U32 => 4,
        TypeKind::I64 | TypeKind::U64 => 8,
        TypeKind::Isize | TypeKind::Usize => 8, // Pointer-sized: 8 bytes on 64-bit targets
        TypeKind::F16 => 2,
        TypeKind::F32 => 4,
        TypeKind::F64 => 8,
        TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => 8, // 64-bit target
        TypeKind::Struct(id) => {
            // Compute LLVM non-packed struct layout: fields are placed at
            // aligned offsets, and the struct is tail-padded to its alignment.
            let def = type_pool.struct_def(id);
            let mut offset = 0u64;
            let mut max_align = 1u64;
            for f in &def.fields {
                let field_align = type_alignment(f.ty, type_pool);
                let field_size = type_byte_size(f.ty, type_pool);
                if field_size == 0 {
                    continue; // zero-sized fields don't participate
                }
                max_align = max_align.max(field_align);
                // Pad to field alignment
                offset = (offset + field_align - 1) & !(field_align - 1);
                offset += field_size;
            }
            // Tail padding to struct alignment
            if max_align > 1 {
                offset = (offset + max_align - 1) & !(max_align - 1);
            }
            offset
        }
        TypeKind::Array(id) => {
            let (elem_ty, len) = type_pool.array_def(id);
            type_byte_size(elem_ty, type_pool) * len
        }
        TypeKind::Enum(id) => {
            let def = type_pool.enum_def(id);
            if def.is_unit_only() {
                type_byte_size(def.discriminant_type(), type_pool)
            } else {
                // Tagged union: { discrim_type, [max_payload x i8] }
                // Since [N x i8] has alignment 1, there is no padding between
                // discriminant and payload. Tail padding may apply if discrim
                // alignment > 1.
                let discrim_size = type_byte_size(def.discriminant_type(), type_pool);
                let discrim_align = type_alignment(def.discriminant_type(), type_pool);
                let max_payload: u64 = def
                    .variants
                    .iter()
                    .map(|v| {
                        v.fields
                            .iter()
                            .map(|f| type_byte_size(*f, type_pool))
                            .sum::<u64>()
                    })
                    .max()
                    .unwrap_or(0);
                if max_payload == 0 {
                    discrim_size
                } else {
                    let total = discrim_size + max_payload;
                    // Tail padding to struct alignment
                    (total + discrim_align - 1) & !(discrim_align - 1)
                }
            }
        }
        _ => 0, // Unit, Never, ComptimeType, Module, Error
    }
}

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

        // Enums:
        // - Unit-only enums: represented as their discriminant integer type (backward compat).
        // - Data enums: tagged union `{ discriminant_type, [max_payload_bytes x i8] }`.
        TypeKind::Enum(id) => {
            let def = type_pool.enum_def(id);
            if def.is_unit_only() {
                // C-style enum: just the discriminant integer.
                gruel_type_to_llvm(def.discriminant_type(), ctx, type_pool)
            } else {
                // Data enum: tagged union struct.
                let discrim_llvm = gruel_type_to_llvm(def.discriminant_type(), ctx, type_pool)?;
                let max_payload: u64 = def
                    .variants
                    .iter()
                    .map(|v| {
                        v.fields
                            .iter()
                            .map(|f| type_byte_size(*f, type_pool))
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

        // Non-code-gen types — not representable in LLVM IR.
        TypeKind::Error
        | TypeKind::ComptimeType
        | TypeKind::ComptimeStr
        | TypeKind::ComptimeInt
        | TypeKind::Module(_) => None,
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

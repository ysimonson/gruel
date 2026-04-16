//! Gruel type → LLVM type mapping.

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

        // Enums are represented as their discriminant integer type.
        TypeKind::Enum(id) => {
            let def = type_pool.enum_def(id);
            gruel_type_to_llvm(def.discriminant_type(), ctx, type_pool)
        }

        // Non-code-gen types — not representable in LLVM IR.
        TypeKind::Error | TypeKind::ComptimeType | TypeKind::Module(_) => None,
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

/// Return the number of native ABI slots that `ty` occupies.
///
/// This mirrors `SemaContext::abi_slot_count` exactly:
/// - Scalars (integers, bool, enum, pointer) → 1
/// - Struct → sum of field slot counts
/// - Array → `len * element_slot_count`
/// - Zero-sized types (unit, never, comptime-only) → 0
///
/// Used by the LLVM backend to map ABI slot indices to LLVM parameter indices.
pub fn abi_slot_count(ty: Type, type_pool: &TypeInternPool) -> u32 {
    match ty.kind() {
        TypeKind::Struct(id) => {
            let def = type_pool.struct_def(id);
            def.fields
                .iter()
                .map(|f| abi_slot_count(f.ty, type_pool))
                .sum()
        }
        TypeKind::Array(id) => {
            let (elem_ty, len) = type_pool.array_def(id);
            abi_slot_count(elem_ty, type_pool) * len as u32
        }
        // Zero-sized / comptime-only types.
        TypeKind::Unit | TypeKind::Never | TypeKind::ComptimeType | TypeKind::Module(_) => 0,
        // All other scalars: i8/i16/i32/i64, u8/u16/u32/u64, bool, enum, ptr → 1.
        _ => 1,
    }
}

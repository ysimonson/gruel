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
            // false = not packed (use Gruel's ABI alignment, not byte-packed)
            Some(ctx.struct_type(&field_types, false).into())
        }

        TypeKind::Array(id) => {
            let (elem_ty, len) = type_pool.array_def(id);
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

        // Non-code-gen types — not representable in LLVM IR.
        TypeKind::Error | TypeKind::ComptimeType | TypeKind::Enum(_) | TypeKind::Module(_) => None,
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

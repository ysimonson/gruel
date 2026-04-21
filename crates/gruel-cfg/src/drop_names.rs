//! Shared drop-glue utilities: reachability checks and synthesized symbol names.
//!
//! Both the drop-glue synthesizer (`gruel-compiler`) and the LLVM codegen
//! (`gruel-codegen-llvm`) need to agree on:
//! - whether a type requires a destructor call, and
//! - the exact name of the synthesized `__gruel_drop_*` symbol for that type.
//!
//! Putting these here avoids duplicating the logic and keeps the two sites in
//! sync automatically — if the names ever need to change, there is one place to
//! edit.

use gruel_air::{Type, TypeInternPool, TypeKind};

/// Return `true` if `ty` requires a drop call when it goes out of scope.
///
/// Primitive scalars, unit-only enums, pointers, and unit/never are trivially droppable.
/// A struct needs drop if it has a destructor or if any field needs drop.
/// An array needs drop if its element type needs drop.
/// A data enum needs drop if any variant has a field that needs drop.
pub fn type_needs_drop(ty: Type, type_pool: &TypeInternPool) -> bool {
    match ty.kind() {
        TypeKind::Struct(id) => {
            let def = type_pool.struct_def(id);
            if def.destructor.is_some() {
                return true;
            }
            def.fields.iter().any(|f| type_needs_drop(f.ty, type_pool))
        }
        TypeKind::Array(id) => {
            let (elem, _) = type_pool.array_def(id);
            type_needs_drop(elem, type_pool)
        }
        TypeKind::Enum(id) => {
            let def = type_pool.enum_def(id);
            def.variants
                .iter()
                .any(|v| v.fields.iter().any(|f| type_needs_drop(*f, type_pool)))
        }
        _ => false,
    }
}

/// Return the name of the synthesized `__gruel_drop_*` function for `ty`,
/// or `None` if the type is trivially droppable or is a built-in with a
/// runtime-provided destructor (e.g. `String` → `__gruel_drop_String` lives
/// in `gruel-runtime` and is called via a dedicated code path, not through
/// a synthesized wrapper).
pub fn drop_fn_name(ty: Type, type_pool: &TypeInternPool) -> Option<String> {
    match ty.kind() {
        TypeKind::Struct(id) => {
            let def = type_pool.struct_def(id);
            // Built-in types with runtime destructors are handled by the
            // is_builtin_string / runtime-call path in codegen, not by a
            // synthesized wrapper.
            if def.is_builtin && def.destructor.is_some() {
                return None;
            }
            if type_needs_drop(ty, type_pool) {
                Some(format!("__gruel_drop_{}", def.name))
            } else {
                None
            }
        }
        TypeKind::Array(id) => {
            if type_needs_drop(ty, type_pool) {
                let (elem, len) = type_pool.array_def(id);
                Some(format!(
                    "__gruel_drop_array_{}_{}",
                    type_name_component(elem, type_pool),
                    len
                ))
            } else {
                None
            }
        }
        TypeKind::Enum(id) => {
            if type_needs_drop(ty, type_pool) {
                let def = type_pool.enum_def(id);
                Some(format!("__gruel_drop_{}", def.name))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// A stable, human-readable name component for `ty`, used inside drop-glue
/// symbol names (e.g. the `String_3` in `__gruel_drop_array_String_3`).
pub fn type_name_component(ty: Type, type_pool: &TypeInternPool) -> String {
    match ty.kind() {
        TypeKind::I8 => "i8".to_owned(),
        TypeKind::I16 => "i16".to_owned(),
        TypeKind::I32 => "i32".to_owned(),
        TypeKind::I64 => "i64".to_owned(),
        TypeKind::U8 => "u8".to_owned(),
        TypeKind::U16 => "u16".to_owned(),
        TypeKind::U32 => "u32".to_owned(),
        TypeKind::U64 => "u64".to_owned(),
        TypeKind::I128 => "i128".to_owned(),
        TypeKind::U128 => "u128".to_owned(),
        TypeKind::Bool => "bool".to_owned(),
        TypeKind::Unit => "unit".to_owned(),
        TypeKind::Never => "never".to_owned(),
        TypeKind::Error => "error".to_owned(),
        TypeKind::ComptimeType => "comptime_type".to_owned(),
        TypeKind::ComptimeStr => "comptime_str".to_owned(),
        TypeKind::Enum(id) => format!("enum{}", id.0),
        TypeKind::Struct(id) => type_pool.struct_def(id).name.clone(),
        TypeKind::Array(id) => {
            let (elem, len) = type_pool.array_def(id);
            format!("array_{}_{}", type_name_component(elem, type_pool), len)
        }
        TypeKind::Module(id) => format!("module{}", id.0),
        TypeKind::PtrConst(id) => {
            let pointee = type_pool.ptr_const_def(id);
            format!("ptr_const_{}", type_name_component(pointee, type_pool))
        }
        TypeKind::PtrMut(id) => {
            let pointee = type_pool.ptr_mut_def(id);
            format!("ptr_mut_{}", type_name_component(pointee, type_pool))
        }
    }
}

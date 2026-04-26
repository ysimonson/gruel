//! Built-in type injection for semantic analysis.
//!
//! This module handles injection of built-in types like String as synthetic
//! structs, and built-in enums like Arch and Os as synthetic enums.
//! Built-in types are registered before user code is processed,
//! enabling collision detection and proper type resolution.

use gruel_builtins::{BUILTIN_ENUMS, BUILTIN_TYPES, BuiltinFieldType, BuiltinTypeDef};

use super::Sema;
use crate::types::{
    EnumDef, EnumVariantDef, IfaceTy, InterfaceDef, InterfaceId, InterfaceMethodReq, ReceiverMode,
    StructDef, StructField, StructId, Type, TypeKind,
};

impl<'a> Sema<'a> {
    /// Phase 0: Inject built-in types as synthetic structs and enums.
    ///
    /// This creates `StructDef` entries for built-in types like `String` and
    /// `EnumDef` entries for built-in enums like `Arch` and `Os` before
    /// processing user code. The built-in types are registered in the `structs`
    /// and `enums` HashMaps so they can be looked up by name, and their IDs are
    /// stored in dedicated fields for fast access.
    ///
    /// Built-in types are marked with `is_builtin: true` and have their fields,
    /// destructor, and copy status derived from the `gruel-builtins` registry.
    pub(crate) fn inject_builtin_types(&mut self) {
        // Inject built-in struct types (String, etc.)
        for builtin in BUILTIN_TYPES {
            // Convert builtin field types to our Type enum
            let fields: Vec<StructField> = builtin
                .fields
                .iter()
                .map(|f| StructField {
                    name: f.name.to_string(),
                    ty: match f.ty {
                        BuiltinFieldType::U64 => Type::U64,
                        BuiltinFieldType::U8 => Type::U8,
                        BuiltinFieldType::Bool => Type::BOOL,
                    },
                })
                .collect();

            // Create the synthetic struct definition
            // Built-in types are always public and have no source file
            let struct_def = StructDef {
                name: builtin.name.to_string(),
                fields,
                is_copy: builtin.is_copy,
                is_handle: false, // Built-in types don't use @handle yet
                is_linear: false, // Built-in types are not linear
                destructor: builtin.drop_fn.map(|s| s.to_string()),
                is_builtin: true,
                is_pub: true,                        // Built-in types are always public
                file_id: gruel_span::FileId::new(0), // Synthetic, no source file
            };

            // Register in type pool and get pool-based StructId
            let name_spur = self.interner.get_or_intern(builtin.name);
            let (struct_id, _) = self.type_pool.register_struct(name_spur, struct_def);

            // Register in struct lookup with pool-based StructId
            self.structs.insert(name_spur, struct_id);

            // Store special IDs for quick access
            if builtin.name == "String" {
                self.builtin_string_id = Some(struct_id);
            }

            // Note: Associated functions and methods are not registered here.
            // They are handled by looking up methods in the builtin registry
            // when analyzing method calls on builtin types.
        }

        // Inject built-in enum types (Arch, Os)
        for builtin_enum in BUILTIN_ENUMS {
            // Built-in enums only have unit variants (no associated data).
            let variants: Vec<EnumVariantDef> = builtin_enum
                .variants
                .iter()
                .map(|v| EnumVariantDef::unit(*v))
                .collect();

            // Create the synthetic enum definition
            let enum_def = EnumDef {
                name: builtin_enum.name.to_string(),
                variants,
                is_pub: true,                        // Built-in enums are always public
                file_id: gruel_span::FileId::new(0), // Synthetic, no source file
                destructor: None,
            };

            // Register in type pool and get pool-based EnumId
            let name_spur = self.interner.get_or_intern(builtin_enum.name);
            let (enum_id, _) = self.type_pool.register_enum(name_spur, enum_def);

            // Register in enum lookup
            self.enums.insert(name_spur, enum_id);

            // Store special IDs for quick access
            if builtin_enum.name == "Arch" {
                self.builtin_arch_id = Some(enum_id);
            } else if builtin_enum.name == "Os" {
                self.builtin_os_id = Some(enum_id);
            } else if builtin_enum.name == "TypeKind" {
                self.builtin_typekind_id = Some(enum_id);
            } else if builtin_enum.name == "Ownership" {
                self.builtin_ownership_id = Some(enum_id);
            }
        }

        // Inject the compiler-recognized `Drop` and `Copy` interfaces
        // (ADR-0059). Drop has `fn drop(self)`; Copy has
        // `fn copy(borrow self) -> Self`. The shapes are referenced by the
        // ownership trichotomy and by `@derive(Copy)` validation.
        self.inject_builtin_interfaces();
    }

    /// Register `Drop` and `Copy` as compiler-recognized interfaces. Called
    /// from `inject_builtin_types` so the names are already resolvable when
    /// user code is parsed.
    fn inject_builtin_interfaces(&mut self) {
        let drop_name = self.interner.get_or_intern_static("Drop");
        if !self.interfaces.contains_key(&drop_name) {
            let id = InterfaceId(self.interface_defs.len() as u32);
            self.interface_defs.push(InterfaceDef {
                name: "Drop".to_string(),
                methods: vec![InterfaceMethodReq {
                    name: "drop".to_string(),
                    receiver: ReceiverMode::ByValue,
                    param_types: Vec::new(),
                    return_type: IfaceTy::Concrete(Type::UNIT),
                }],
                is_pub: true,
                file_id: gruel_span::FileId::new(0),
            });
            self.interfaces.insert(drop_name, id);
        }

        let copy_name = self.interner.get_or_intern_static("Copy");
        if !self.interfaces.contains_key(&copy_name) {
            let id = InterfaceId(self.interface_defs.len() as u32);
            self.interface_defs.push(InterfaceDef {
                name: "Copy".to_string(),
                methods: vec![InterfaceMethodReq {
                    name: "copy".to_string(),
                    receiver: ReceiverMode::Borrow,
                    param_types: Vec::new(),
                    return_type: IfaceTy::SelfType,
                }],
                is_pub: true,
                file_id: gruel_span::FileId::new(0),
            });
            self.interfaces.insert(copy_name, id);
        }
    }

    // ========================================================================
    // Builtin type helper methods
    // ========================================================================

    /// Check if a type is the builtin String type.
    ///
    /// Uses the stored `builtin_string_id` for fast comparison.
    pub(crate) fn is_builtin_string(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Struct(struct_id) => Some(struct_id) == self.builtin_string_id,
            _ => false,
        }
    }

    /// Get the builtin type definition for a struct if it's a builtin type.
    ///
    /// Returns `Some(&BuiltinTypeDef)` if the struct is a builtin type,
    /// `None` otherwise.
    pub(crate) fn get_builtin_type_def(
        &self,
        struct_id: StructId,
    ) -> Option<&'static BuiltinTypeDef> {
        let struct_def = self.type_pool.struct_def(struct_id);
        if struct_def.is_builtin {
            gruel_builtins::get_builtin_type(&struct_def.name)
        } else {
            None
        }
    }

    /// Get the String struct type.
    ///
    /// Returns the Type::Struct for the builtin String type.
    /// Panics if called before builtin types are injected.
    pub(crate) fn builtin_string_type(&self) -> Type {
        Type::new_struct(
            self.builtin_string_id
                .expect("builtin types not injected yet"),
        )
    }

    /// Check if a method name is a builtin mutation method.
    ///
    /// Mutation methods need special handling because they require storage location
    /// to be captured before the receiver is analyzed.
    pub(crate) fn is_builtin_mutation_method(&self, method_name: &str) -> bool {
        use gruel_builtins::ReceiverMode;

        // Check all builtin types for methods with ByMutRef receiver
        for builtin in BUILTIN_TYPES {
            if let Some(method) = builtin.find_method(method_name)
                && method.receiver_mode == ReceiverMode::ByMutRef
            {
                return true;
            }
        }
        false
    }

    /// Get the AIR output type for a builtin struct.
    ///
    /// Builtin types like String are now represented as Type::Struct with is_builtin=true.
    pub(crate) fn builtin_air_type(&self, struct_id: StructId) -> Type {
        Type::new_struct(struct_id)
    }

    /// Check if a type is a linear type.
    /// Only struct types can be linear - primitives and other types are not linear.
    pub(crate) fn is_type_linear(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Struct(struct_id) => {
                let struct_def = self.type_pool.struct_def(struct_id);
                struct_def.is_linear
            }
            // Only struct types can be linear
            _ => false,
        }
    }

    /// Variant index of the `Ownership` builtin enum classifying `ty`.
    ///
    /// Mirrors the `Ownership` variant order in `gruel-builtins`:
    /// `Copy` = 0, `Affine` = 1, `Linear` = 2.
    pub(crate) fn ownership_variant_index(&self, ty: Type) -> u32 {
        if self.is_type_linear(ty) {
            2
        } else if self.is_type_copy(ty) {
            0
        } else {
            1
        }
    }
}

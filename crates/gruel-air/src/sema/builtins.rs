//! Built-in type injection for semantic analysis.
//!
//! This module handles injection of built-in types like String as synthetic
//! structs, and built-in enums like Arch and Os as synthetic enums.
//! Built-in types are registered before user code is processed,
//! enabling collision detection and proper type resolution.

use gruel_builtins::{BUILTIN_TYPES, BuiltinFieldType, BuiltinTypeDef};

use super::Sema;
use crate::types::{StructDef, StructField, StructId, Type, TypeKind};

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
                    ty: self.resolve_builtin_field_type(f.ty),
                    // ADR-0073: built-ins are homed in `<builtin>`, so
                    // non-pub fields are unreachable from user code via
                    // the unified `is_accessible` check.
                    is_pub: f.is_pub,
                })
                .collect();

            // Create the synthetic struct definition
            // Built-in types are always public and have no source file
            let struct_def = StructDef {
                name: builtin.name.to_string(),
                fields,
                is_copy: builtin.is_copy,
                is_clone: false,
                is_linear: false, // Built-in types are not linear
                destructor: builtin.drop_fn.map(|s| s.to_string()),
                is_builtin: true,
                is_pub: true, // Built-in types are always public
                // ADR-0073: built-ins live in a sentinel "builtin module"
                // that user code is never part of. Non-pub fields and
                // methods are unreachable from user code by the unified
                // visibility check (`is_accessible(user_file, BUILTIN, ...)`).
                file_id: gruel_util::FileId::BUILTIN,
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

        // ADR-0078 Phase 2: the compiler-recognized interfaces (Drop, Copy,
        // Clone, Handle) are now declared in `std/prelude/interfaces.gruel`.
        // ADR-0078 Phase 3: the platform-reflection enums (Arch, Os, TypeKind,
        // Ownership) are now declared in `std/prelude/target.gruel`. Both
        // categories register into the standard `self.interfaces` /
        // `self.enums` maps during `resolve_declarations`; the hardcoded
        // behaviors that key off the names continue to find them via
        // `cache_builtin_enum_ids` (called after declaration resolution).
    }

    /// Cache `EnumId`s for the four prelude-resident built-in enums (`Arch`,
    /// `Os`, `TypeKind`, `Ownership`) so the intrinsics that produce values
    /// of those types can build `Type::new_enum(id)` without doing a name
    /// lookup at every call site.
    ///
    /// Called once after `resolve_declarations` has run — by which time the
    /// prelude's enum declarations have been registered into `self.enums`.
    pub(crate) fn cache_builtin_enum_ids(&mut self) {
        if let Some(spur) = self.interner.get("Arch")
            && let Some(&id) = self.enums.get(&spur)
        {
            self.builtin_arch_id = Some(id);
        }
        if let Some(spur) = self.interner.get("Os")
            && let Some(&id) = self.enums.get(&spur)
        {
            self.builtin_os_id = Some(id);
        }
        if let Some(spur) = self.interner.get("TypeKind")
            && let Some(&id) = self.enums.get(&spur)
        {
            self.builtin_typekind_id = Some(id);
        }
        if let Some(spur) = self.interner.get("Ownership")
            && let Some(&id) = self.enums.get(&spur)
        {
            self.builtin_ownership_id = Some(id);
        }
        // ADR-0078 Phase 4: cache `Ordering` for the binop dispatch in
        // `analyze_comparison`, which constructs `Ordering::Less` /
        // `Ordering::Greater` enum-variant AIR refs to compare against the
        // `cmp(self, other)` return value.
        if let Some(spur) = self.interner.get("Ordering")
            && let Some(&id) = self.enums.get(&spur)
        {
            self.builtin_ordering_id = Some(id);
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
    ///
    /// Delegates to `TypeInternPool::is_type_linear`, which is the single
    /// source of truth for linearity semantics (ADR-0067).
    pub(crate) fn is_type_linear(&self, ty: Type) -> bool {
        self.type_pool.is_type_linear(ty)
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

    /// Resolve a `BuiltinFieldType` to a concrete `Type` (ADR-0072).
    ///
    /// Scalar variants (`U64`, `U8`, `Bool`) map directly. The
    /// `BuiltinType("Vec(u8)")` form is the structural newtype reference
    /// introduced for `String::bytes`; the v1 implementation only resolves
    /// the exact spellings it knows about.
    fn resolve_builtin_field_type(&mut self, ty: BuiltinFieldType) -> Type {
        match ty {
            BuiltinFieldType::U64 => Type::U64,
            BuiltinFieldType::U8 => Type::U8,
            BuiltinFieldType::Bool => Type::BOOL,
            BuiltinFieldType::BuiltinType(name) => self.resolve_builtin_type_name(name),
        }
    }

    /// ADR-0072: enforce `checked`-block gating for the String / Vec(u8)
    /// bridge surface. Hardcoded by name because the builtin registry
    /// has no per-method gate today; if more synthetic surfaces want
    /// `checked` annotations the right move is to add a field to
    /// `BuiltinMethod` / `BuiltinAssociatedFn` rather than extending this
    /// match.
    pub(crate) fn check_string_vec_bridge_method_gates(
        &self,
        type_name: &str,
        method_name: &str,
        ctx: &super::context::AnalysisContext,
        span: gruel_util::Span,
    ) -> gruel_util::CompileResult<()> {
        if type_name != "String" {
            return Ok(());
        }
        // Subset that requires a `checked` block (caller assumes UTF-8
        // invariant or raw-pointer responsibility).
        let checked_gated = matches!(
            method_name,
            "from_utf8_unchecked" | "from_c_str_unchecked" | "push_byte" | "terminated_ptr"
        );
        if checked_gated {
            Self::require_checked_for_intrinsic(ctx, method_name, span)?;
        }
        Ok(())
    }

    /// Resolve a source-form built-in type name (e.g. `"Vec(u8)"`) to its
    /// concrete `Type`. Used by `BuiltinFieldType::BuiltinType`,
    /// `BuiltinParamType::BuiltinType`, and `BuiltinReturnType::BuiltinType`.
    pub(crate) fn resolve_builtin_type_name(&mut self, name: &str) -> Type {
        match name {
            "Vec(u8)" => {
                let vec_id = self.type_pool.intern_vec_from_type(Type::U8);
                Type::new_vec(vec_id)
            }
            "Ptr(u8)" => {
                let id = self.type_pool.intern_ptr_const_from_type(Type::U8);
                Type::new_ptr_const(id)
            }
            "MutPtr(u8)" => {
                let id = self.type_pool.intern_ptr_mut_from_type(Type::U8);
                Type::new_ptr_mut(id)
            }
            other => panic!(
                "unsupported builtin type reference {:?}; \
                 extend `Sema::resolve_builtin_type_name` to support it",
                other
            ),
        }
    }
}

//! Built-in type / enum lookups for semantic analysis.
//!
//! ADR-0081 retired the `BUILTIN_TYPES` registry — `String` is now
//! a regular prelude struct. The remaining responsibilities of this
//! module are caching `StructId` / `EnumId`s for prelude-resident
//! types the compiler hard-codes by name (`String`, `Arch`, `Os`,
//! `TypeKind`, `Ownership`, `Ordering`) and the `String`/`Vec(u8)`
//! bridge `checked`-block gates from ADR-0072.

use super::Sema;
use crate::types::{Type, TypeKind};

impl<'a> Sema<'a> {
    /// Phase 0 hook for built-in injection. ADR-0081 left
    /// `BUILTIN_TYPES` empty; this is now a no-op kept around so the
    /// pipeline order in `analyze_all` reads consistently.
    pub(crate) fn inject_builtin_types(&mut self) {}

    /// Cache `StructId` / `EnumId`s for prelude-resident types the
    /// compiler hard-codes by name. Called once after
    /// `resolve_declarations` has run — by which time the prelude's
    /// declarations have been registered into `self.structs` /
    /// `self.enums`.
    pub(crate) fn cache_builtin_enum_ids(&mut self) {
        // ADR-0081: the prelude `String` struct id powers the
        // `is_builtin_string` / `builtin_string_type` fast lookups
        // (string-literal lowering, intrinsic return types, etc.).
        if let Some(spur) = self.interner.get("String")
            && let Some(&id) = self.structs.get(&spur)
        {
            self.builtin_string_id = Some(id);
        }

        // ADR-0078 Phase 3: the platform-reflection enums (`Arch`, `Os`)
        // and type-reflection enums (`TypeKind`, `Ownership`) live in
        // the prelude. The intrinsics that produce values of those
        // types read these cached ids without doing a name lookup at
        // every call site.
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
        // ADR-0084: cache the prelude `ThreadSafety` enum so the
        // `@thread_safety` intrinsic can materialize variants without a
        // name lookup at every call site.
        if let Some(spur) = self.interner.get("ThreadSafety")
            && let Some(&id) = self.enums.get(&spur)
        {
            self.builtin_thread_safety_id = Some(id);
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

    /// Check if a type is the prelude `String` struct.
    ///
    /// Uses the cached `builtin_string_id` for fast comparison.
    pub(crate) fn is_builtin_string(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Struct(struct_id) => Some(struct_id) == self.builtin_string_id,
            _ => false,
        }
    }

    /// Get the String struct type.
    ///
    /// Returns `Type::new_struct` for the prelude-resident `String` if the
    /// prelude has been loaded; otherwise returns `Type::ERROR`. Callers
    /// (e.g., string-literal lowering, `@parse_*` argument validation,
    /// `@read_line`) propagate the error type so the compile errors out
    /// cleanly instead of panicking when a test fixture skips the
    /// prelude.
    pub(crate) fn builtin_string_type(&self) -> Type {
        match self.builtin_string_id {
            Some(id) => Type::new_struct(id),
            None => Type::ERROR,
        }
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

    /// ADR-0084: variant index of the `ThreadSafety` builtin enum
    /// classifying `ty`.
    ///
    /// Variant order in `prelude/type_info.gruel`: `Unsend` = 0,
    /// `Send` = 1, `Sync` = 2 — matches the compiler-side `ThreadSafety`
    /// enum's derived `Ord` impl.
    pub(crate) fn thread_safety_variant_index(&self, ty: Type) -> u32 {
        match self.type_pool.is_thread_safety_type(ty) {
            gruel_builtins::ThreadSafety::Unsend => 0,
            gruel_builtins::ThreadSafety::Send => 1,
            gruel_builtins::ThreadSafety::Sync => 2,
        }
    }

    /// ADR-0088: returns true if the given directive list contains
    /// `@mark(unchecked)`. Used to fire the
    /// `UncheckedFnExtensions` preview gate at method declaration sites
    /// and to detect FFI imports missing the directive.
    pub(crate) fn directives_have_mark_unchecked(
        &self,
        directives_start: u32,
        directives_len: u32,
    ) -> bool {
        let directives = self.rir.get_directives(directives_start, directives_len);
        directives.iter().any(|d| {
            self.interner.resolve(&d.name) == "mark"
                && d.args
                    .iter()
                    .any(|a| self.interner.resolve(a) == "unchecked")
        })
    }
}

//! Structural conformance check for interfaces (ADR-0056).
//!
//! Given a candidate type and an interface, decide whether the type's method
//! set covers the interface's required methods (matching name, parameter
//! types, and return type). On success, return a `ConformanceWitness`
//! mapping each interface slot to the concrete method on the candidate type;
//! on failure, return a structured `CompileError` describing the gap.
//!
//! The witness is the input both to:
//! - **Comptime constraints (Phase 3)**: lets monomorphization resolve method
//!   calls on the bound type to the concrete method.
//! - **Runtime dispatch (Phase 4)**: drives vtable generation for the
//!   `(concrete type, interface)` pair.

use gruel_util::Span;
use gruel_util::{
    CompileError, CompileResult, ErrorKind, InterfaceMethodMissingData,
    InterfaceMethodSignatureMismatchData,
};
use lasso::Spur;

use super::Sema;
use crate::types::{IfaceTy, InterfaceId, StructId, Type, TypeKind};

/// Witness that a concrete type conforms to an interface.
///
/// `slot_methods[i]` is the candidate type's method that satisfies the
/// interface's i-th required method. The slot order is the interface's
/// declaration order (vtable order).
///
/// Currently only used by Phase 2 unit tests; Phase 3 will consume it during
/// monomorphization, and Phase 4 during vtable generation.
#[derive(Debug, Clone)]
pub struct ConformanceWitness {
    /// Resolved methods, one per interface slot. Each entry is the
    /// candidate's method name as a Spur and its `(StructId, Spur)` key into
    /// `Sema::methods`.
    pub slot_methods: Vec<(StructId, Spur)>,
}

impl<'a> Sema<'a> {
    /// Check whether `candidate` conforms to `interface_id`.
    ///
    /// Returns a witness on success, or a `CompileError` on the first failing
    /// requirement. (We could collect all failures into one error for a
    /// richer report; for the MVP, fail-fast is fine — later phases can
    /// extend this if the diagnostic ergonomics demand it.)
    ///
    /// `use_span` is reported as the location of the conformance check
    /// failure; the caller typically passes the span of the use site
    /// (a generic call or coercion) rather than the interface declaration.
    pub(crate) fn check_conforms(
        &self,
        candidate: Type,
        interface_id: InterfaceId,
        use_span: Span,
    ) -> CompileResult<ConformanceWitness> {
        // ADR-0059 / ADR-0079: `Copy` and `Drop` are compiler-recognized
        // via lang-items, not by the literal interface name. Short-circuit
        // through the existing ownership predicates so primitives,
        // pointers, arrays, etc. don't need synthetic method tables —
        // codegen handles built-in copy/drop natively.
        if Some(interface_id) == self.lang_items.copy() {
            return self.check_copy_conformance(candidate, interface_id, use_span);
        }
        if Some(interface_id) == self.lang_items.drop() {
            return self.check_drop_conformance(candidate, interface_id, use_span);
        }
        if Some(interface_id) == self.lang_items.clone()
            && let Some(witness) = self.check_clone_short_circuit(candidate, interface_id, use_span)
        {
            return witness;
        }

        let candidate_struct_id = match candidate.kind() {
            TypeKind::Struct(id) => id,
            // For Phase 2, only struct candidates are supported. Enums get
            // their own method table in `Sema::enum_methods`; once enum
            // conformance is added, add an arm here. Pointers, arrays,
            // primitives etc. don't have impl blocks.
            _ => {
                let iface_def = &self.interface_defs[interface_id.0 as usize];
                let type_name = self.format_type_name(candidate);
                let interface_name = iface_def.name.clone();
                // Phrase as "missing method" for the first required slot —
                // that's the most actionable error: there's no method set at
                // all on the candidate.
                if let Some(req) = iface_def.methods.first() {
                    return Err(CompileError::new(
                        ErrorKind::InterfaceMethodMissing(Box::new(InterfaceMethodMissingData {
                            type_name,
                            interface_name,
                            method_name: req.name.clone(),
                            expected_signature: self
                                .format_interface_method_sig(interface_id, &req.name),
                        })),
                        use_span,
                    ));
                }
                // Empty interface — every type trivially conforms.
                return Ok(ConformanceWitness {
                    slot_methods: Vec::new(),
                });
            }
        };

        // Take a snapshot of relevant interface info to avoid borrowing
        // issues while we build the witness.
        let iface_def = self.interface_defs[interface_id.0 as usize].clone();
        let mut slot_methods = Vec::with_capacity(iface_def.methods.len());

        for req in &iface_def.methods {
            // Look up the method by name on the candidate.
            let method_name_sym = match self.interner.get(&req.name) {
                Some(s) => s,
                None => {
                    // The method name was never interned — meaning no method
                    // on any type has this name. Definitely missing.
                    return Err(self.iface_method_missing(
                        candidate,
                        interface_id,
                        &req.name,
                        use_span,
                    ));
                }
            };

            let method_info = match self.methods.get(&(candidate_struct_id, method_name_sym)) {
                Some(info) => *info,
                None => {
                    return Err(self.iface_method_missing(
                        candidate,
                        interface_id,
                        &req.name,
                        use_span,
                    ));
                }
            };

            // Method must be a method (have self), not an associated function.
            if !method_info.has_self {
                return Err(self.iface_method_sig_mismatch(
                    candidate,
                    interface_id,
                    &req.name,
                    "associated function (no `self`)",
                    use_span,
                ));
            }

            // Receiver mode must match exactly (ADR-0060).
            if method_info.receiver != req.receiver {
                return Err(self.iface_method_sig_mismatch(
                    candidate,
                    interface_id,
                    &req.name,
                    &self.format_concrete_method_sig(&method_info),
                    use_span,
                ));
            }

            // Compare parameter types (excluding self) and return type.
            let candidate_param_types = self.param_arena.types(method_info.params);
            if candidate_param_types.len() != req.param_types.len() {
                return Err(self.iface_method_sig_mismatch(
                    candidate,
                    interface_id,
                    &req.name,
                    &self.format_concrete_method_sig(&method_info),
                    use_span,
                ));
            }
            for (req_ty, cand_ty) in req.param_types.iter().zip(candidate_param_types.iter()) {
                if req_ty.substitute_self(candidate) != *cand_ty {
                    return Err(self.iface_method_sig_mismatch(
                        candidate,
                        interface_id,
                        &req.name,
                        &self.format_concrete_method_sig(&method_info),
                        use_span,
                    ));
                }
            }
            if method_info.return_type != req.return_type.substitute_self(candidate) {
                return Err(self.iface_method_sig_mismatch(
                    candidate,
                    interface_id,
                    &req.name,
                    &self.format_concrete_method_sig(&method_info),
                    use_span,
                ));
            }

            slot_methods.push((candidate_struct_id, method_name_sym));
        }

        Ok(ConformanceWitness { slot_methods })
    }

    /// Built-in conformance for the `Copy` interface (ADR-0059).
    ///
    /// Linear types never conform; otherwise `is_type_copy` is the source of
    /// truth. The witness has no real slot — the compiler dispatches Copy
    /// for built-ins natively, and `@derive(Copy)` user types reach the
    /// regular method-set path on later phases.
    fn check_copy_conformance(
        &self,
        candidate: Type,
        interface_id: InterfaceId,
        use_span: Span,
    ) -> CompileResult<ConformanceWitness> {
        if self.is_type_linear(candidate) {
            return Err(self.iface_method_missing(candidate, interface_id, "copy", use_span));
        }
        if self.is_type_copy(candidate) {
            return Ok(ConformanceWitness {
                slot_methods: Vec::new(),
            });
        }
        Err(self.iface_method_missing(candidate, interface_id, "copy", use_span))
    }

    /// Built-in short-circuit for the `Clone` interface (ADR-0065).
    ///
    /// Returns `Some(...)` to short-circuit the regular method-table check:
    /// - Linear types never conform.
    /// - Copy types automatically conform (bitwise-copy synthesis).
    /// - Built-in types with a `clone` method (e.g. `String`) conform via
    ///   the built-in method registry.
    ///
    /// Returns `None` to fall through to the regular method-table check
    /// (used for affine user types that have written `fn clone(borrow self)
    /// -> Self` themselves or via `@derive(Clone)`).
    fn check_clone_short_circuit(
        &self,
        candidate: Type,
        interface_id: InterfaceId,
        use_span: Span,
    ) -> Option<CompileResult<ConformanceWitness>> {
        if self.is_type_linear(candidate) {
            return Some(Err(self.iface_method_missing(
                candidate,
                interface_id,
                "clone",
                use_span,
            )));
        }
        if self.is_type_copy(candidate) {
            return Some(Ok(ConformanceWitness {
                slot_methods: Vec::new(),
            }));
        }
        if let TypeKind::Struct(struct_id) = candidate.kind()
            && let Some(builtin) = self.get_builtin_type_def(struct_id)
            && builtin.find_method("clone").is_some()
        {
            return Some(Ok(ConformanceWitness {
                slot_methods: Vec::new(),
            }));
        }
        // ADR-0065 Phase 2: `@derive(Clone)` structs have an `is_clone` flag
        // and a synthesized `<TypeName>.clone` function emitted by
        // `clone_glue`. The conformance witness needs no real method slot —
        // dispatch is handled by a parallel short-circuit in
        // `analyze_method_call_impl`.
        if let TypeKind::Struct(struct_id) = candidate.kind() {
            let struct_def = self.type_pool.struct_def(struct_id);
            if struct_def.is_clone {
                return Some(Ok(ConformanceWitness {
                    slot_methods: Vec::new(),
                }));
            }
        }
        None
    }

    /// Built-in conformance for the `Drop` interface (ADR-0059).
    ///
    /// Affine types (non-`Copy`, non-linear) conform — they all have a
    /// drop-on-scope-exit, either user-written via `fn drop(self)` or the
    /// compiler-synthesized recursive drop.
    fn check_drop_conformance(
        &self,
        candidate: Type,
        interface_id: InterfaceId,
        use_span: Span,
    ) -> CompileResult<ConformanceWitness> {
        if self.is_type_linear(candidate) {
            return Err(self.iface_method_missing(candidate, interface_id, "drop", use_span));
        }
        if self.is_type_copy(candidate) {
            return Err(self.iface_method_missing(candidate, interface_id, "drop", use_span));
        }
        Ok(ConformanceWitness {
            slot_methods: Vec::new(),
        })
    }

    fn iface_method_missing(
        &self,
        candidate: Type,
        interface_id: InterfaceId,
        method_name: &str,
        span: Span,
    ) -> CompileError {
        let iface_def = &self.interface_defs[interface_id.0 as usize];
        CompileError::new(
            ErrorKind::InterfaceMethodMissing(Box::new(InterfaceMethodMissingData {
                type_name: self.format_type_name(candidate),
                interface_name: iface_def.name.clone(),
                method_name: method_name.to_string(),
                expected_signature: self.format_interface_method_sig(interface_id, method_name),
            })),
            span,
        )
    }

    fn iface_method_sig_mismatch(
        &self,
        candidate: Type,
        interface_id: InterfaceId,
        method_name: &str,
        found_signature: &str,
        span: Span,
    ) -> CompileError {
        let iface_def = &self.interface_defs[interface_id.0 as usize];
        CompileError::new(
            ErrorKind::InterfaceMethodSignatureMismatch(Box::new(
                InterfaceMethodSignatureMismatchData {
                    type_name: self.format_type_name(candidate),
                    interface_name: iface_def.name.clone(),
                    method_name: method_name.to_string(),
                    expected_signature: self.format_interface_method_sig(interface_id, method_name),
                    found_signature: found_signature.to_string(),
                },
            )),
            span,
        )
    }

    fn format_iface_ty(&self, t: &IfaceTy) -> String {
        match t {
            IfaceTy::SelfType => "Self".to_string(),
            IfaceTy::Concrete(ty) => self.format_type_name(*ty),
        }
    }

    fn format_interface_method_sig(&self, interface_id: InterfaceId, method_name: &str) -> String {
        let iface_def = &self.interface_defs[interface_id.0 as usize];
        let req = match iface_def.methods.iter().find(|m| m.name == method_name) {
            Some(r) => r,
            None => return format!("fn {}(self)", method_name),
        };
        let recv = req.receiver.render();
        let params: Vec<String> = req
            .param_types
            .iter()
            .map(|t| self.format_iface_ty(t))
            .collect();
        let prefix = if params.is_empty() {
            format!("fn {}({})", method_name, recv)
        } else {
            format!("fn {}({}, {})", method_name, recv, params.join(", "))
        };
        match &req.return_type {
            IfaceTy::Concrete(t) if *t == Type::UNIT => prefix,
            other => format!("{} -> {}", prefix, self.format_iface_ty(other)),
        }
    }

    fn format_concrete_method_sig(&self, method_info: &super::MethodInfo) -> String {
        let param_types = self.param_arena.types(method_info.params);
        let recv = method_info.receiver.render();
        let params: Vec<String> = param_types
            .iter()
            .map(|t| self.format_type_name(*t))
            .collect();
        let prefix = if params.is_empty() {
            format!("fn({})", recv)
        } else {
            format!("fn({}, {})", recv, params.join(", "))
        };
        if method_info.return_type == Type::UNIT {
            prefix
        } else {
            format!(
                "{} -> {}",
                prefix,
                self.format_type_name(method_info.return_type)
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sema::Sema;
    use gruel_lexer::Lexer;
    use gruel_parser::Parser;
    use gruel_rir::AstGen;
    use gruel_util::PreviewFeatures;
    use gruel_util::Span;

    /// Like `gather_declarations_for_testing` in `sema::tests`, but with
    /// the `interfaces` preview feature enabled and `validate_interface_decls`
    /// run so the interface table is populated.
    fn gather(source: &str) -> Sema<'static> {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, interner) = parser.parse().unwrap();

        let astgen = AstGen::new(&ast, &interner);
        let rir = astgen.generate();

        let rir = Box::leak(Box::new(rir));
        let interner = Box::leak(Box::new(interner));

        let preview = PreviewFeatures::default();
        let mut sema = Sema::new(rir, interner, preview);
        sema.inject_builtin_types();
        sema.register_type_names().unwrap();
        // resolve_declarations now runs validate_interface_decls internally
        // (between struct/enum field resolution and method gathering).
        sema.resolve_declarations().unwrap();
        sema
    }

    fn iface_id(sema: &Sema<'_>, name: &str) -> InterfaceId {
        let sym = sema.interner.get(name).expect("interface name interned");
        *sema.interfaces.get(&sym).expect("interface registered")
    }

    fn struct_ty(sema: &Sema<'_>, name: &str) -> Type {
        let sym = sema.interner.get(name).expect("struct name interned");
        let id = *sema.structs.get(&sym).expect("struct registered");
        Type::new_struct(id)
    }

    fn dummy_span() -> Span {
        Span::new(0, 0)
    }

    #[test]
    fn conforms_basic() {
        // Use a non-`drop` method name: `drop` is special-cased as the
        // destructor (ADR-0053) and therefore not stored in `Sema::methods`.
        let sema = gather(
            r#"
            interface Greeter {
                fn greet(self);
            }

            struct Foo {
                fn greet(self) {}
            }

            fn main() -> i32 { 0 }
            "#,
        );
        let iid = iface_id(&sema, "Greeter");
        let foo = struct_ty(&sema, "Foo");
        let witness = sema
            .check_conforms(foo, iid, dummy_span())
            .expect("conforms");
        assert_eq!(witness.slot_methods.len(), 1);
    }

    #[test]
    fn conforms_with_params_and_return() {
        let sema = gather(
            r#"
            interface Reader {
                fn read(self, n: i32) -> i32;
            }

            struct Buf {
                fn read(self, n: i32) -> i32 { n }
            }

            fn main() -> i32 { 0 }
            "#,
        );
        let iid = iface_id(&sema, "Reader");
        let buf = struct_ty(&sema, "Buf");
        sema.check_conforms(buf, iid, dummy_span())
            .expect("conforms");
    }

    #[test]
    fn missing_method_rejected() {
        let sema = gather(
            r#"
            interface Greeter {
                fn greet(self);
            }

            struct Foo {}

            fn main() -> i32 { 0 }
            "#,
        );
        let iid = iface_id(&sema, "Greeter");
        let foo = struct_ty(&sema, "Foo");
        let err = sema
            .check_conforms(foo, iid, dummy_span())
            .expect_err("should fail");
        match err.kind {
            ErrorKind::InterfaceMethodMissing(data) => {
                assert_eq!(data.method_name, "greet");
            }
            other => panic!("expected InterfaceMethodMissing, got {:?}", other),
        }
    }

    #[test]
    fn return_type_mismatch_rejected() {
        let sema = gather(
            r#"
            interface Reader {
                fn read(self, n: i32) -> i32;
            }

            struct Buf {
                fn read(self, n: i32) -> i64 { 0 }
            }

            fn main() -> i32 { 0 }
            "#,
        );
        let iid = iface_id(&sema, "Reader");
        let buf = struct_ty(&sema, "Buf");
        let err = sema
            .check_conforms(buf, iid, dummy_span())
            .expect_err("should fail");
        assert!(matches!(
            err.kind,
            ErrorKind::InterfaceMethodSignatureMismatch(_)
        ));
    }

    #[test]
    fn arity_mismatch_rejected() {
        let sema = gather(
            r#"
            interface Reader {
                fn read(self, n: i32) -> i32;
            }

            struct Buf {
                fn read(self) -> i32 { 0 }
            }

            fn main() -> i32 { 0 }
            "#,
        );
        let iid = iface_id(&sema, "Reader");
        let buf = struct_ty(&sema, "Buf");
        let err = sema
            .check_conforms(buf, iid, dummy_span())
            .expect_err("should fail");
        assert!(matches!(
            err.kind,
            ErrorKind::InterfaceMethodSignatureMismatch(_)
        ));
    }

    #[test]
    fn self_return_type_substitutes_candidate() {
        // ADR-0060: `Self` in return position resolves to the candidate type.
        let sema = gather(
            r#"
            interface Cloner {
                fn clone(self) -> Self;
            }

            struct Foo {
                fn clone(self) -> Foo { Foo {} }
            }

            fn main() -> i32 { 0 }
            "#,
        );
        let iid = iface_id(&sema, "Cloner");
        let foo = struct_ty(&sema, "Foo");
        sema.check_conforms(foo, iid, dummy_span())
            .expect("conforms");
    }

    #[test]
    fn self_return_type_mismatch_rejected() {
        // ADR-0060: a candidate whose return type is not the candidate itself
        // fails to conform when the interface declares `-> Self`.
        let sema = gather(
            r#"
            interface Cloner {
                fn clone(self) -> Self;
            }

            struct Foo {
                fn clone(self) -> i32 { 0 }
            }

            fn main() -> i32 { 0 }
            "#,
        );
        let iid = iface_id(&sema, "Cloner");
        let foo = struct_ty(&sema, "Foo");
        let err = sema
            .check_conforms(foo, iid, dummy_span())
            .expect_err("should fail");
        assert!(matches!(
            err.kind,
            ErrorKind::InterfaceMethodSignatureMismatch(_)
        ));
    }

    #[test]
    fn self_param_type_substitutes_candidate() {
        // `Self` in a non-receiver parameter position binds to the candidate.
        let sema = gather(
            r#"
            interface Combiner {
                fn combine(self, other: Self) -> Self;
            }

            struct Foo {
                fn combine(self, other: Foo) -> Foo { other }
            }

            fn main() -> i32 { 0 }
            "#,
        );
        let iid = iface_id(&sema, "Combiner");
        let foo = struct_ty(&sema, "Foo");
        sema.check_conforms(foo, iid, dummy_span())
            .expect("conforms");
    }

    #[test]
    fn self_param_type_mismatch_rejected() {
        let sema = gather(
            r#"
            interface Combiner {
                fn combine(self, other: Self) -> Self;
            }

            struct Foo {
                fn combine(self, other: i32) -> Foo { Foo {} }
            }

            fn main() -> i32 { 0 }
            "#,
        );
        let iid = iface_id(&sema, "Combiner");
        let foo = struct_ty(&sema, "Foo");
        let err = sema
            .check_conforms(foo, iid, dummy_span())
            .expect_err("should fail");
        assert!(matches!(
            err.kind,
            ErrorKind::InterfaceMethodSignatureMismatch(_)
        ));
    }

    #[test]
    fn receiver_mode_must_match() {
        // ADR-0060/0076: candidate's `self: Ref(Self)` matches interface's
        // `self: Ref(Self)`.
        let sema = gather(
            r#"
            interface Reader {
                fn read(self: Ref(Self)) -> i32;
            }

            struct Buf {
                fn read(self: Ref(Self)) -> i32 { 0 }
            }

            fn main() -> i32 { 0 }
            "#,
        );
        let iid = iface_id(&sema, "Reader");
        let buf = struct_ty(&sema, "Buf");
        sema.check_conforms(buf, iid, dummy_span())
            .expect("conforms");
    }

    #[test]
    fn receiver_mode_mismatch_rejected() {
        // ADR-0060/0076: by-value `self` does not satisfy `self: Ref(Self)`.
        let sema = gather(
            r#"
            interface Reader {
                fn read(self: Ref(Self)) -> i32;
            }

            struct Buf {
                fn read(self) -> i32 { 0 }
            }

            fn main() -> i32 { 0 }
            "#,
        );
        let iid = iface_id(&sema, "Reader");
        let buf = struct_ty(&sema, "Buf");
        let err = sema
            .check_conforms(buf, iid, dummy_span())
            .expect_err("should fail");
        assert!(matches!(
            err.kind,
            ErrorKind::InterfaceMethodSignatureMismatch(_)
        ));
    }

    #[test]
    fn empty_interface_trivially_conforms() {
        let sema = gather(
            r#"
            interface Marker {}
            struct Foo {}

            fn main() -> i32 { 0 }
            "#,
        );
        let iid = iface_id(&sema, "Marker");
        let foo = struct_ty(&sema, "Foo");
        let witness = sema
            .check_conforms(foo, iid, dummy_span())
            .expect("conforms");
        assert!(witness.slot_methods.is_empty());
    }
}

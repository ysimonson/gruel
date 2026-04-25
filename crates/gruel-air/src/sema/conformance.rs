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

use gruel_error::{
    CompileError, CompileResult, ErrorKind, InterfaceMethodMissingData,
    InterfaceMethodSignatureMismatchData,
};
use gruel_span::Span;
use lasso::Spur;

use super::Sema;
use crate::types::{InterfaceId, StructId, Type, TypeKind};

/// Witness that a concrete type conforms to an interface.
///
/// `slot_methods[i]` is the candidate type's method that satisfies the
/// interface's i-th required method. The slot order is the interface's
/// declaration order (vtable order).
///
/// Currently only used by Phase 2 unit tests; Phase 3 will consume it during
/// monomorphization, and Phase 4 during vtable generation.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ConformanceWitness {
    /// Resolved methods, one per interface slot. Each entry is the
    /// candidate's method name as a Spur and its `(StructId, Spur)` key into
    /// `Sema::methods`.
    pub slot_methods: Vec<(StructId, Spur)>,
}

#[allow(dead_code)] // exercised by Phase 2 unit tests; consumed by Phase 3+
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
                if *req_ty != *cand_ty {
                    return Err(self.iface_method_sig_mismatch(
                        candidate,
                        interface_id,
                        &req.name,
                        &self.format_concrete_method_sig(&method_info),
                        use_span,
                    ));
                }
            }
            if method_info.return_type != req.return_type {
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

    fn format_interface_method_sig(&self, interface_id: InterfaceId, method_name: &str) -> String {
        let iface_def = &self.interface_defs[interface_id.0 as usize];
        let req = match iface_def.methods.iter().find(|m| m.name == method_name) {
            Some(r) => r,
            None => return format!("fn {}(self)", method_name),
        };
        let params: Vec<String> = req
            .param_types
            .iter()
            .map(|t| self.format_type_name(*t))
            .collect();
        let prefix = if params.is_empty() {
            format!("fn {}(self)", method_name)
        } else {
            format!("fn {}(self, {})", method_name, params.join(", "))
        };
        if req.return_type == Type::UNIT {
            prefix
        } else {
            format!("{} -> {}", prefix, self.format_type_name(req.return_type))
        }
    }

    fn format_concrete_method_sig(&self, method_info: &super::MethodInfo) -> String {
        let param_types = self.param_arena.types(method_info.params);
        let params: Vec<String> = param_types
            .iter()
            .map(|t| self.format_type_name(*t))
            .collect();
        let prefix = if params.is_empty() {
            "fn(self)".to_string()
        } else {
            format!("fn(self, {})", params.join(", "))
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
    use gruel_error::PreviewFeatures;
    use gruel_lexer::Lexer;
    use gruel_parser::Parser;
    use gruel_rir::AstGen;
    use gruel_span::Span;

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

        let preview = PreviewFeatures::new();
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

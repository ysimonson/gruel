//! Declaration gathering for semantic analysis.
//!
//! This module handles the first phase of semantic analysis: gathering all
//! type and function declarations from the RIR. This includes:
//!
//! - Registering struct and enum type names
//! - Resolving struct field types
//! - Collecting function signatures
//! - Collecting method signatures from impl blocks
//! - Validating @derive(Copy) and @handle structs

use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

use gruel_builtins::{is_reserved_type_constructor_name, is_reserved_type_name};
use gruel_rir::{InstData, InstRef, RirParamMode};
use gruel_util::Span;
use gruel_util::{CompileError, CompileResult, ErrorKind, ice};
use lasso::Spur;

use super::anon_interfaces::decode_receiver_mode;
use super::{ConstInfo, FunctionInfo, InferenceContext, MethodInfo, Sema};
use crate::inference::{FunctionSig, MethodSig};
use crate::types::{
    EnumDef, EnumId, EnumVariantDef, InterfaceDef, InterfaceId, InterfaceMethodReq, ReceiverMode,
    StructDef, StructField, StructId, Type,
};

/// Posture declared by the user on a type definition (ADR-0080).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclaredPosture {
    Copy,
    Affine,
    Linear,
}

impl DeclaredPosture {
    fn as_str(self) -> &'static str {
        match self {
            DeclaredPosture::Copy => "copy",
            DeclaredPosture::Affine => "affine",
            DeclaredPosture::Linear => "linear",
        }
    }
}

/// Posture observed on a member's type (ADR-0080).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemberPosture {
    Copy,
    Affine,
    Linear,
}

impl MemberPosture {
    fn as_str(self) -> &'static str {
        match self {
            MemberPosture::Copy => "Copy",
            MemberPosture::Affine => "affine",
            MemberPosture::Linear => "linear",
        }
    }
}

/// ADR-0080: compare a member's posture against the host's declared
/// posture and produce a `PostureMismatch` error if the rule is violated.
///
/// Rules (with `Linear` short-circuited by the caller, since linear hosts
/// accept anything):
///
/// - `Copy` host: every member must be Copy.
/// - `Affine` host: no member may be Linear.
fn check_posture_against_declared(
    host_kind: &'static str,
    host_name: &str,
    declared: DeclaredPosture,
    member_kind: &'static str,
    member_name: &str,
    member_type: &str,
    member_posture: MemberPosture,
    span: Span,
) -> Option<CompileError> {
    let violates = match (declared, member_posture) {
        (DeclaredPosture::Copy, MemberPosture::Affine | MemberPosture::Linear) => true,
        (DeclaredPosture::Affine, MemberPosture::Linear) => true,
        _ => false,
    };
    if !violates {
        return None;
    }
    Some(CompileError::new(
        ErrorKind::PostureMismatch(Box::new(gruel_util::PostureMismatchError {
            host_kind,
            host_name: host_name.to_string(),
            declared_posture: declared.as_str(),
            member_kind,
            member_name: member_name.to_string(),
            member_type: member_type.to_string(),
            member_posture: member_posture.as_str(),
        })),
        span,
    ))
}

/// Signature data for a `drop` method, bundled for inline-drop registration.
struct DropSignature {
    has_self: bool,
    params_len: u32,
    return_type: Spur,
    body: InstRef,
    span: Span,
}

/// Visibility / safety flags on a top-level function declaration.
#[derive(Clone, Copy)]
struct FnFlags {
    is_pub: bool,
    is_unchecked: bool,
}

impl<'a> Sema<'a> {
    /// Build an `InferenceContext` from the collected type information.
    ///
    /// This should be called after the collection phase and builds the
    /// pre-computed maps needed for Hindley-Milner type inference.
    /// Building this once and reusing for all function analyses avoids
    /// the O(n²) cost of rebuilding these maps per function.
    ///
    /// # Performance
    ///
    /// This converts all function/method signatures to use `InferType`
    /// (which handles arrays structurally rather than by ID). This conversion
    /// is done once instead of per-function.
    pub fn build_inference_context(&self) -> InferenceContext {
        // Build function signatures with InferType for constraint generation
        let func_sigs: HashMap<Spur, FunctionSig> = self
            .functions
            .iter()
            .map(|(name, info)| {
                (
                    *name,
                    FunctionSig {
                        param_types: self
                            .param_arena
                            .types(info.params)
                            .iter()
                            .map(|t| self.type_to_infer_type(*t))
                            .collect(),
                        return_type: self.type_to_infer_type(info.return_type),
                        is_generic: info.is_generic,
                        param_modes: self.param_arena.modes(info.params).to_vec(),
                        param_comptime: self.param_arena.comptime(info.params).to_vec(),
                        param_names: self.param_arena.names(info.params).to_vec(),
                        return_type_sym: info.return_type_sym,
                    },
                )
            })
            .collect();

        // Build struct types map (name -> Type::new_struct(id))
        let struct_types: HashMap<Spur, Type> = self
            .structs
            .iter()
            .map(|(name, id)| (*name, Type::new_struct(*id)))
            .collect();

        // Build enum types map (name -> Type::new_enum(id))
        let enum_types: HashMap<Spur, Type> = self
            .enums
            .iter()
            .map(|(name, id)| (*name, Type::new_enum(*id)))
            .collect();

        // Build method signatures with InferType for constraint generation.
        // Method-level-generic methods (ADR-0055) are excluded: their stored
        // `return_type` is a `COMPTIME_TYPE` placeholder, and their param
        // types may reference unresolved method-level type params. The
        // inference pass falls back to fresh type variables for calls to
        // these methods, and the specialized bodies (synthesized by
        // `specialize::create_specialized_method`) are analyzed separately
        // with the concrete substitutions in place.
        let method_sigs: HashMap<(StructId, Spur), MethodSig> = self
            .methods
            .iter()
            .filter(|(_, info)| !info.is_generic)
            .map(|((struct_id, method_name), info)| {
                (
                    (*struct_id, *method_name),
                    MethodSig {
                        struct_type: info.struct_type,
                        has_self: info.has_self,
                        param_types: self
                            .param_arena
                            .types(info.params)
                            .iter()
                            .map(|t| self.type_to_infer_type(*t))
                            .collect(),
                        return_type: self.type_to_infer_type(info.return_type),
                    },
                )
            })
            .collect();

        // Build enum method signatures for constraint generation
        let enum_method_sigs: HashMap<(EnumId, Spur), MethodSig> = self
            .enum_methods
            .iter()
            .map(|((enum_id, method_name), info)| {
                (
                    (*enum_id, *method_name),
                    MethodSig {
                        struct_type: info.struct_type,
                        has_self: info.has_self,
                        param_types: self
                            .param_arena
                            .types(info.params)
                            .iter()
                            .map(|t| self.type_to_infer_type(*t))
                            .collect(),
                        return_type: self.type_to_infer_type(info.return_type),
                    },
                )
            })
            .collect();

        InferenceContext {
            func_sigs,
            struct_types,
            enum_types,
            method_sigs,
            enum_method_sigs,
        }
    }
    // ADR-0080 retired `has_copy_directive`: `@derive(Copy)` no longer
    // resolves to a known interface, so the directive falls through the
    // existing "unknown interface" derive resolver path.

    /// ADR-0075: walk every directive collected during parsing and reject any
    /// whose name isn't in the recognized set (`@allow`, `@derive`). Retired
    /// directives (`@handle`, `@copy`) get a targeted retirement note; other
    /// unknowns get an edit-distance suggestion when there's a near-match.
    pub(crate) fn validate_directives(&self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            let (start, len) = match &inst.data {
                InstData::StructDecl {
                    directives_start,
                    directives_len,
                    ..
                }
                | InstData::FnDecl {
                    directives_start,
                    directives_len,
                    ..
                }
                | InstData::Alloc {
                    directives_start,
                    directives_len,
                    ..
                }
                | InstData::ConstDecl {
                    directives_start,
                    directives_len,
                    ..
                }
                | InstData::AnonStructType {
                    directives_start,
                    directives_len,
                    ..
                }
                | InstData::AnonEnumType {
                    directives_start,
                    directives_len,
                    ..
                } => (*directives_start, *directives_len),
                _ => continue,
            };
            if len == 0 {
                continue;
            }
            for directive in self.rir.get_directives(start, len) {
                let name = self.interner.resolve(&directive.name).to_string();
                if name == "allow" || name == "derive" || name == "lang" {
                    continue;
                }
                let note = directive_diagnosis_note(&name);
                return Err(CompileError::new(
                    ErrorKind::UnknownDirective { name, note },
                    directive.span,
                ));
            }
        }
        Ok(())
    }

    /// Phase 1: Register all type names (enum and struct IDs).
    ///
    /// This creates name → ID mappings for all enums and structs in a single pass,
    /// allowing types to reference each other in any order. Struct definitions are
    /// created with placeholder empty fields that will be filled in during phase 2.
    /// Validate and register `interface` declarations (ADR-0056).
    ///
    /// - Gates each declaration behind the `interfaces` preview feature.
    /// - Rejects duplicate method names within a single interface.
    /// - Rejects collisions between interface names and existing structs/enums
    ///   (and other interfaces).
    /// - Resolves each method signature's parameter and return types and
    ///   stores an `InterfaceDef` on `Sema`.
    ///
    /// Conformance and dispatch wiring land in later phases.
    pub(crate) fn validate_interface_decls(&mut self) -> CompileResult<()> {
        // Two-pass walk: first collect raw decls (snapshotting RIR data so we
        // can mutate Sema while resolving types), then register each one.
        struct RawIface {
            name: Spur,
            is_pub: bool,
            file_id: gruel_util::FileId,
            decl_span: Span,
            methods: Vec<RawIfaceMethod>,
        }
        struct RawIfaceMethod {
            name: Spur,
            params: Vec<(Spur, Spur)>, // (param_name, type_symbol)
            return_type_sym: Spur,
            receiver: ReceiverMode,
            span: Span,
        }

        let mut raw: Vec<RawIface> = Vec::new();
        for (_, inst) in self.rir.iter() {
            if let InstData::InterfaceDecl {
                is_pub,
                name,
                methods_start,
                methods_len,
                directives_start: _,
                directives_len: _,
            } = &inst.data
            {
                let method_refs = self.rir.get_inst_refs(*methods_start, *methods_len);
                let mut seen: HashSet<Spur> = HashSet::default();
                let mut methods = Vec::new();
                for method_ref in method_refs {
                    let m = self.rir.get(method_ref);
                    if let InstData::InterfaceMethodSig {
                        name: method_name,
                        params_start,
                        params_len,
                        return_type,
                        receiver_mode,
                    } = &m.data
                    {
                        if !seen.insert(*method_name) {
                            let iface_name = self.interner.resolve(name).to_string();
                            let method_name_str = self.interner.resolve(method_name).to_string();
                            return Err(CompileError::new(
                                ErrorKind::DuplicateMethod {
                                    type_name: format!("interface `{}`", iface_name),
                                    method_name: method_name_str,
                                },
                                m.span,
                            ));
                        }
                        let params = self
                            .rir
                            .get_params(*params_start, *params_len)
                            .into_iter()
                            .map(|p| (p.name, p.ty))
                            .collect();
                        let receiver = match *receiver_mode {
                            1 => ReceiverMode::MutRef,
                            2 => ReceiverMode::Ref,
                            _ => ReceiverMode::ByValue,
                        };
                        methods.push(RawIfaceMethod {
                            name: *method_name,
                            params,
                            return_type_sym: *return_type,
                            receiver,
                            span: m.span,
                        });
                    }
                }

                raw.push(RawIface {
                    name: *name,
                    is_pub: *is_pub,
                    file_id: inst.span.file_id,
                    decl_span: inst.span,
                    methods,
                });
            }
        }

        for r in raw {
            // Reject collision with existing structs/enums/interfaces.
            let name_str = self.interner.resolve(&r.name).to_string();
            if self.structs.contains_key(&r.name)
                || self.enums.contains_key(&r.name)
                || self.interfaces.contains_key(&r.name)
            {
                return Err(CompileError::new(
                    ErrorKind::DuplicateTypeDefinition {
                        type_name: format!("interface `{}`", name_str),
                    },
                    r.decl_span,
                ));
            }

            // Resolve each method's parameter and return slots, recognizing
            // the symbol `Self` as `IfaceTy::SelfType` (ADR-0060). Receiver
            // mode is `ByValue` until Phase 2 threads `inout`/`borrow self`
            // from the parser.
            let mut resolved_methods = Vec::with_capacity(r.methods.len());
            for m in &r.methods {
                let mut param_types = Vec::with_capacity(m.params.len());
                for (_pname, ty_sym) in &m.params {
                    param_types.push(self.resolve_iface_ty(*ty_sym, m.span)?);
                }
                let return_type = self.resolve_iface_ty(m.return_type_sym, m.span)?;
                resolved_methods.push(InterfaceMethodReq {
                    name: self.interner.resolve(&m.name).to_string(),
                    receiver: m.receiver,
                    param_types,
                    return_type,
                });
            }

            let id = InterfaceId(self.interface_defs.len() as u32);
            self.interface_defs.push(InterfaceDef {
                name: name_str,
                methods: resolved_methods,
                is_pub: r.is_pub,
                file_id: r.file_id,
            });
            self.interfaces.insert(r.name, id);
        }

        Ok(())
    }

    pub(crate) fn register_type_names(&mut self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            match &inst.data {
                InstData::EnumDecl {
                    is_pub,
                    is_copy,
                    is_linear,
                    name,
                    variants_start,
                    variants_len,
                    ..
                } => {
                    let enum_name = self.interner.resolve(name).to_string();

                    // ADR-0080 belt-and-braces: parser already rejects
                    // `copy linear` / `linear copy` syntactically; this catches
                    // the path where one is a keyword and the other arrives
                    // via `@derive(Copy)`.
                    if *is_copy && *is_linear {
                        return Err(CompileError::new(
                            ErrorKind::LinearStructCopy(enum_name.clone()),
                            inst.span,
                        ));
                    }

                    // Check for collision with built-in type names or built-in
                    // type constructors (e.g. Ptr, MutPtr — see ADR-0061).
                    if is_reserved_type_name(&enum_name)
                        || is_reserved_type_constructor_name(&enum_name)
                    {
                        return Err(CompileError::new(
                            ErrorKind::ReservedTypeName {
                                type_name: enum_name,
                            },
                            inst.span,
                        ));
                    }

                    // Check for duplicate type definitions (struct or enum with same name)
                    if self.enums.contains_key(name) || self.structs.contains_key(name) {
                        return Err(CompileError::new(
                            ErrorKind::DuplicateTypeDefinition {
                                type_name: enum_name,
                            },
                            inst.span,
                        ));
                    }

                    let raw_variants = self
                        .rir
                        .get_enum_variant_decls(*variants_start, *variants_len);

                    // Check for duplicate variant names
                    let mut seen_variants: HashSet<Spur> = HashSet::default();
                    for (variant_name, _, field_names) in &raw_variants {
                        if !seen_variants.insert(*variant_name) {
                            let variant_name_str = self.interner.resolve(variant_name).to_string();
                            return Err(CompileError::new(
                                ErrorKind::DuplicateVariant {
                                    enum_name: enum_name.clone(),
                                    variant_name: variant_name_str,
                                },
                                inst.span,
                            ));
                        }

                        if !field_names.is_empty() {
                            // Check for duplicate field names within the struct variant
                            let variant_str = self.interner.resolve(variant_name).to_string();
                            let mut seen_fields: HashSet<Spur> = HashSet::default();
                            for field_name in field_names {
                                if !seen_fields.insert(*field_name) {
                                    let field_str = self.interner.resolve(field_name).to_string();
                                    return Err(CompileError::new(
                                        ErrorKind::DuplicateField {
                                            struct_name: format!("{}::{}", enum_name, variant_str),
                                            field_name: field_str,
                                        },
                                        inst.span,
                                    ));
                                }
                            }
                        }
                    }

                    // Build EnumVariantDef list. Field types are stored as unit for now;
                    // full type resolution will be added in later phases when we
                    // lower them through the type checker.
                    let variants: Vec<EnumVariantDef> = raw_variants
                        .iter()
                        .map(|(vname, _fields, field_names)| EnumVariantDef {
                            name: self.interner.resolve(vname).to_string(),
                            fields: Vec::new(), // Field types resolved in later phases
                            field_names: field_names
                                .iter()
                                .map(|n| self.interner.resolve(n).to_string())
                                .collect(),
                        })
                        .collect();

                    let enum_def = EnumDef {
                        name: enum_name,
                        variants,
                        is_copy: *is_copy,
                        is_linear: *is_linear,
                        is_pub: *is_pub,
                        file_id: inst.span.file_id,
                        destructor: None,
                    };

                    // Register in type pool and get pool-based EnumId
                    let (enum_id, _) = self.type_pool.register_enum(*name, enum_def);

                    // Register in enum lookup with pool-based EnumId
                    self.enums.insert(*name, enum_id);
                }
                InstData::StructDecl {
                    directives_start,
                    directives_len,
                    is_pub,
                    is_copy: kw_is_copy,
                    is_linear,
                    name,
                    ..
                } => {
                    let struct_name = self.interner.resolve(name).to_string();

                    // Check for collision with built-in type names or built-in
                    // type constructors (e.g. Ptr, MutPtr — see ADR-0061).
                    if is_reserved_type_name(&struct_name)
                        || is_reserved_type_constructor_name(&struct_name)
                    {
                        return Err(CompileError::new(
                            ErrorKind::ReservedTypeName {
                                type_name: struct_name,
                            },
                            inst.span,
                        ));
                    }

                    // Check for duplicate type definitions (struct or enum with same name)
                    if self.structs.contains_key(name) || self.enums.contains_key(name) {
                        return Err(CompileError::new(
                            ErrorKind::DuplicateTypeDefinition {
                                type_name: struct_name,
                            },
                            inst.span,
                        ));
                    }

                    let _ = *directives_start;
                    let _ = *directives_len;
                    // ADR-0080 Phase 5: `@derive(Copy)` is retired; the
                    // `copy` keyword is now the sole source of truth for
                    // `StructDef.is_copy`.
                    let is_copy = *kw_is_copy;

                    // Linear types cannot be @derive(Copy)
                    if *is_linear && is_copy {
                        return Err(CompileError::new(
                            ErrorKind::LinearStructCopy(struct_name.clone()),
                            inst.span,
                        ));
                    }

                    // Create placeholder struct def (fields will be resolved in phase 2)
                    let struct_def = StructDef {
                        name: struct_name,
                        fields: Vec::new(), // Filled in during resolve_declarations
                        is_copy,
                        is_clone: false, // Filled in during resolve_declarations after fields known
                        is_linear: *is_linear,
                        destructor: None,  // Filled in during resolve_declarations
                        is_builtin: false, // User-defined struct
                        is_pub: *is_pub,
                        file_id: inst.span.file_id,
                    };

                    // Register in type pool and get pool-based StructId
                    let (struct_id, _) = self.type_pool.register_struct(*name, struct_def);

                    // Register in struct lookup with pool-based StructId
                    self.structs.insert(*name, struct_id);
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Phase 2: Resolve all declarations.
    ///
    /// Now that all type names are registered, this resolves:
    /// - Struct field types (must be done first for @derive(Copy) validation)
    /// - @derive(Copy) struct validation, destructors, functions, and methods
    ///
    /// # Array Type Registration
    ///
    /// Array types from explicit type annotations (struct fields, function parameters,
    /// return types, local variable annotations) are registered during this phase via
    /// `resolve_type()` calls. Array types from literals (inferred during HM inference)
    /// are created on-demand via the thread-safe `TypeInternPool` during function
    /// body analysis.
    pub(crate) fn resolve_declarations(&mut self) -> CompileResult<()> {
        // ADR-0075: surface unrecognized `@name` directives before any
        // downstream pass that would silently ignore them.
        self.validate_directives()?;
        // Derives (ADR-0058) are gated by the `comptime_derives` preview
        // feature. Reject any `derive` item before we touch the rest of
        // declaration gathering so users get a clear diagnostic.
        self.validate_derive_decls()?;
        // Interfaces (ADR-0056) must be registered before struct/enum field
        // and method-param resolution so that:
        //   - using an interface name as a struct field type produces a
        //     helpful diagnostic redirecting to the comptime path
        //   - `comptime T: SomeInterface` bounds are recognized when
        //     functions and methods are gathered
        //   - `borrow t: SomeInterface` parameter types resolve correctly
        self.validate_interface_decls()?;
        // ADR-0079: bind `@lang("…")` directives in the prelude to
        // interface/enum IDs. Must run after interfaces are registered
        // and before any sema pass that consults `lang_items`. Enum
        // bindings only resolve once `register_type_names` has populated
        // `self.enums`, which already happened in Phase 1.
        self.populate_lang_items()?;
        self.resolve_struct_fields()?;
        self.resolve_enum_variant_fields()?;
        // ADR-0080 Phase 3: validate that each named declaration's
        // declared posture (`copy` / unmarked / `linear`) is consistent
        // with the postures of its fields/variants. Runs after field
        // resolution so member types are populated. Anonymous types are
        // checked at construction time (see `find_or_create_anon_struct`
        // / `find_or_create_anon_enum`).
        self.validate_posture_consistency()?;
        // ADR-0058 sub-phase: resolve every `@derive(D)` directive on a
        // named struct or enum into a binding for the upcoming expansion
        // step. Runs after field-type resolution so the host type is
        // already fully known.
        self.resolve_derive_directives()?;
        // ADR-0058 sub-phase: splice every `(host_type, derive_id)`
        // binding's methods into the host's method list. Runs after the
        // host's fields are known so destructor / Copy validation
        // (`resolve_remaining_declarations`) sees the attached methods.
        self.expand_derives()?;
        self.resolve_remaining_declarations()?;
        Ok(())
    }

    /// Phase 4 of ADR-0058: walk every recorded `DeriveBinding` and splice
    /// the named derive's methods into the host type's method list. The
    /// same routine is reused at the anonymous-host call site (see
    /// `splice_derive_methods_into_struct`).
    pub(crate) fn expand_derives(&mut self) -> CompileResult<()> {
        // Snapshot the bindings so we can iterate while mutating
        // `self.methods`. Bindings are restored after expansion so the
        // analysis loop can drive each spliced method's body analysis.
        let bindings = self.derive_bindings.clone();
        for b in &bindings {
            // Anonymous-host expansion uses a different call site; named
            // bindings always carry a `host_name` that resolves through
            // `self.structs` (or, in the future, `self.enums`).
            if b.host_is_enum {
                let enum_id = match self.enums.get(&b.host_name).copied() {
                    Some(id) => id,
                    None => continue,
                };
                self.splice_derive_methods_into_enum(b.derive_name, enum_id, b.directive_span)?;
            } else {
                let struct_id = match self.structs.get(&b.host_name).copied() {
                    Some(id) => id,
                    None => continue,
                };
                self.splice_derive_methods_into_struct(b.derive_name, struct_id, b.directive_span)?;
            }
        }
        // Replace the (now-consumed) bindings — the cloned working set is
        // already done. Phase 4 keeps the original list around for the
        // analysis loop to drive body analysis later.
        self.derive_bindings = bindings;
        Ok(())
    }

    /// Anonymous-host call site for ADR-0058: walk an anonymous-struct
    /// expression's `@derive(...)` directives and splice each derive's
    /// methods into the freshly-built host. Should be called exactly once
    /// per new anonymous `StructId` (the structural-dedup path skips this).
    pub(crate) fn splice_anon_struct_derives(
        &mut self,
        host_id: StructId,
        directives_start: u32,
        directives_len: u32,
    ) -> CompileResult<()> {
        if directives_len == 0 {
            return Ok(());
        }
        let derive_dir_sym = self.interner.get_or_intern("derive");
        let directives = self.rir.get_directives(directives_start, directives_len);
        for d in directives {
            if d.name != derive_dir_sym {
                continue;
            }
            if d.args.len() != 1 {
                return Err(CompileError::new(
                    ErrorKind::DeriveNotADerive {
                        name: "<wrong arg count>".to_string(),
                        found: format!("{} arguments", d.args.len()),
                    },
                    d.span,
                ));
            }
            let derive_name = d.args[0];
            // ADR-0059: `@derive(Copy)` on an anonymous host is no-op for
            // method splicing; the `is_copy` bookkeeping flows through the
            // existing copy-directive path.
            if self.is_compiler_derive(derive_name) {
                continue;
            }
            let name_str = self.interner.resolve(&derive_name).to_string();
            if !self.derives.contains_key(&derive_name) {
                let found = if self.structs.contains_key(&derive_name) {
                    "struct"
                } else if self.enums.contains_key(&derive_name) {
                    "enum"
                } else if self.interfaces.contains_key(&derive_name) {
                    "interface"
                } else if self.functions.contains_key(&derive_name) {
                    "function"
                } else {
                    "unknown name"
                };
                return Err(CompileError::new(
                    ErrorKind::DeriveNotADerive {
                        name: name_str,
                        found: found.to_string(),
                    },
                    d.span,
                ));
            }
            self.splice_derive_methods_into_struct(derive_name, host_id, d.span)?;
        }
        Ok(())
    }

    /// Anonymous-enum mirror of `splice_anon_struct_derives`.
    pub(crate) fn splice_anon_enum_derives(
        &mut self,
        host_id: EnumId,
        directives_start: u32,
        directives_len: u32,
    ) -> CompileResult<()> {
        if directives_len == 0 {
            return Ok(());
        }
        let derive_dir_sym = self.interner.get_or_intern("derive");
        let directives = self.rir.get_directives(directives_start, directives_len);
        for d in directives {
            if d.name != derive_dir_sym {
                continue;
            }
            if d.args.len() != 1 {
                return Err(CompileError::new(
                    ErrorKind::DeriveNotADerive {
                        name: "<wrong arg count>".to_string(),
                        found: format!("{} arguments", d.args.len()),
                    },
                    d.span,
                ));
            }
            let derive_name = d.args[0];
            // ADR-0059: `@derive(Copy)` short-circuits — no methods to splice.
            if self.is_compiler_derive(derive_name) {
                continue;
            }
            let name_str = self.interner.resolve(&derive_name).to_string();
            if !self.derives.contains_key(&derive_name) {
                let found = if self.structs.contains_key(&derive_name) {
                    "struct"
                } else if self.enums.contains_key(&derive_name) {
                    "enum"
                } else if self.interfaces.contains_key(&derive_name) {
                    "interface"
                } else if self.functions.contains_key(&derive_name) {
                    "function"
                } else {
                    "unknown name"
                };
                return Err(CompileError::new(
                    ErrorKind::DeriveNotADerive {
                        name: name_str,
                        found: found.to_string(),
                    },
                    d.span,
                ));
            }
            self.splice_derive_methods_into_enum(derive_name, host_id, d.span)?;
        }
        Ok(())
    }

    /// Splice every method of `derive_name` into the struct identified by
    /// `host_id`. `Self` is bound to the host struct's `Type`.
    pub(crate) fn splice_derive_methods_into_struct(
        &mut self,
        derive_name: Spur,
        host_id: StructId,
        directive_span: Span,
    ) -> CompileResult<()> {
        // Snapshot the per-derive method list (we'll borrow `self` mutably
        // when calling `resolve_param_type` below).
        let methods: Vec<crate::sema::info::DeriveMethod> = match self.derives.get(&derive_name) {
            Some(info) => info.methods.clone(),
            None => return Ok(()),
        };
        let host_type = Type::new_struct(host_id);

        // ADR-0079: linear types must not pick up a Clone impl. The
        // prelude `derive Clone` synthesizes a fresh `Self` field-by-
        // field, which would silently duplicate a linear value. Catch
        // this at splice time so the diagnostic points at the
        // `@derive(Clone)` directive rather than at the synthesized
        // body's first @field_set.
        let derive_iface = self.interfaces.get(&derive_name).copied();
        if derive_iface.is_some()
            && derive_iface == self.lang_items.clone()
            && self.type_pool.struct_def(host_id).is_linear
        {
            let host_str = self.type_pool.struct_def(host_id).name.clone();
            return Err(CompileError::new(
                ErrorKind::LinearStructClone(host_str),
                directive_span,
            ));
        }

        // ADR-0076: bind `Self` to the host struct while resolving derived
        // method signatures.
        let saved_self = self.current_self.replace(host_type);

        let host_file_id = self.type_pool.struct_def(host_id).file_id;
        for dm in methods {
            let m = self.rir.get(dm.method_ref);
            let InstData::FnDecl {
                name: method_name,
                is_pub: method_is_pub,
                is_unchecked,
                params_start,
                params_len,
                return_type,
                body,
                has_self,
                receiver_mode,
                ..
            } = m.data
            else {
                continue;
            };
            let receiver = decode_receiver_mode(receiver_mode);
            let key = (host_id, method_name);
            if self.methods.contains_key(&key) {
                let derive_str = self.interner.resolve(&derive_name).to_string();
                let method_str = self.interner.resolve(&method_name).to_string();
                let host_str = self.type_pool.struct_def(host_id).name.clone();
                let prior_span = self.methods.get(&key).map(|info| info.span);
                let mut err = CompileError::new(
                    ErrorKind::DuplicateMethod {
                        type_name: format!(
                            "type `{}` (attached by `@derive({})`)",
                            host_str, derive_str
                        ),
                        method_name: method_str.clone(),
                    },
                    directive_span,
                )
                .with_label(
                    format!("`{}` declared inside `derive {}`", method_str, derive_str),
                    m.span,
                );
                if let Some(s) = prior_span {
                    err = err.with_label(format!("`{}` already attached here", method_str), s);
                }
                return Err(err);
            }

            let params = self.rir.get_params(params_start, params_len);
            let param_names: Vec<Spur> = params.iter().map(|p| p.name).collect();
            let param_comptime: Vec<bool> = params.iter().map(|p| p.is_comptime).collect();
            let mut param_types: Vec<Type> = Vec::with_capacity(params.len());
            let mut param_modes: Vec<gruel_rir::RirParamMode> = Vec::with_capacity(params.len());
            for p in &params {
                let (ty, mode) = self.resolve_param_type(p.ty, p.mode, m.span)?;
                param_types.push(ty);
                param_modes.push(mode);
            }
            let ret_type = self.resolve_type(return_type, m.span)?;
            let param_range =
                self.param_arena
                    .alloc(param_names, param_types, param_modes, param_comptime);

            // ADR-0073: derived methods are scoped to the host's module —
            // the host's file_id is the relevant target_file_id for any
            // visibility check at a call site.
            self.methods.insert(
                key,
                MethodInfo {
                    struct_type: host_type,
                    has_self,
                    receiver,
                    params: param_range,
                    return_type: ret_type,
                    body,
                    span: m.span,
                    is_unchecked,
                    is_generic: false,
                    return_type_sym: return_type,
                    is_pub: method_is_pub,
                    file_id: host_file_id,
                },
            );
        }
        self.current_self = saved_self;
        Ok(())
    }

    /// Splice every method of `derive_name` into the enum identified by
    /// `host_id`. `Self` is bound to the host enum's `Type`. Mirrors the
    /// struct case; structs and enums share the splicing logic (only the
    /// destination map differs).
    pub(crate) fn splice_derive_methods_into_enum(
        &mut self,
        derive_name: Spur,
        host_id: EnumId,
        directive_span: Span,
    ) -> CompileResult<()> {
        let methods: Vec<crate::sema::info::DeriveMethod> = match self.derives.get(&derive_name) {
            Some(info) => info.methods.clone(),
            None => return Ok(()),
        };
        let host_type = Type::new_enum(host_id);
        // ADR-0076: bind `Self` to the host enum while resolving derived
        // method signatures.
        let saved_self = self.current_self.replace(host_type);
        let host_file_id = self.type_pool.enum_def(host_id).file_id;

        for dm in methods {
            let m = self.rir.get(dm.method_ref);
            let InstData::FnDecl {
                name: method_name,
                is_pub: method_is_pub,
                is_unchecked,
                params_start,
                params_len,
                return_type,
                body,
                has_self,
                receiver_mode,
                ..
            } = m.data
            else {
                continue;
            };
            let receiver = decode_receiver_mode(receiver_mode);
            let key = (host_id, method_name);
            if self.enum_methods.contains_key(&key) {
                let derive_str = self.interner.resolve(&derive_name).to_string();
                let method_str = self.interner.resolve(&method_name).to_string();
                let prior_span = self.enum_methods.get(&key).map(|info| info.span);
                let mut err = CompileError::new(
                    ErrorKind::DuplicateMethod {
                        type_name: format!("enum (attached by `@derive({})`)", derive_str),
                        method_name: method_str.clone(),
                    },
                    directive_span,
                )
                .with_label(
                    format!("`{}` declared inside `derive {}`", method_str, derive_str),
                    m.span,
                );
                if let Some(s) = prior_span {
                    err = err.with_label(format!("`{}` already attached here", method_str), s);
                }
                return Err(err);
            }

            let params = self.rir.get_params(params_start, params_len);
            let param_names: Vec<Spur> = params.iter().map(|p| p.name).collect();
            let param_comptime: Vec<bool> = params.iter().map(|p| p.is_comptime).collect();
            let mut param_types: Vec<Type> = Vec::with_capacity(params.len());
            let mut param_modes: Vec<gruel_rir::RirParamMode> = Vec::with_capacity(params.len());
            for p in &params {
                let (ty, mode) = self.resolve_param_type(p.ty, p.mode, m.span)?;
                param_types.push(ty);
                param_modes.push(mode);
            }
            let ret_type = self.resolve_type(return_type, m.span)?;
            let param_range =
                self.param_arena
                    .alloc(param_names, param_types, param_modes, param_comptime);

            self.enum_methods.insert(
                key,
                MethodInfo {
                    struct_type: host_type,
                    has_self,
                    receiver,
                    params: param_range,
                    return_type: ret_type,
                    body,
                    span: m.span,
                    is_unchecked,
                    is_generic: false,
                    return_type_sym: return_type,
                    is_pub: method_is_pub,
                    file_id: host_file_id,
                },
            );
        }
        self.current_self = saved_self;
        Ok(())
    }

    /// Walk every named struct/enum declaration; for each `@derive(D)`
    /// directive, resolve `D` against `Sema::derives` and record a
    /// `DeriveBinding` for Phase 4. Errors if `D` doesn't name a `derive`
    /// item.
    pub(crate) fn resolve_derive_directives(&mut self) -> CompileResult<()> {
        use super::DeriveBinding;

        // Snapshot of struct/enum declarations carrying `@derive(...)`.
        struct RawAttach {
            host_name: Spur,
            host_is_enum: bool,
            host_span: Span,
            derive_names: Vec<(Spur, Span)>,
        }
        let derive_dir_sym = self.interner.get_or_intern("derive");
        let mut raw: Vec<RawAttach> = Vec::new();

        for (_, inst) in self.rir.iter() {
            // ADR-0079 Phase 1 routed directives through both
            // `StructDecl` and `EnumDecl`; collect from both so
            // `@derive(...)` works on enums too.
            let (directives_start, directives_len, name, host_is_enum) = match &inst.data {
                InstData::StructDecl {
                    directives_start,
                    directives_len,
                    name,
                    ..
                } => (*directives_start, *directives_len, *name, false),
                InstData::EnumDecl {
                    directives_start,
                    directives_len,
                    name,
                    ..
                } => (*directives_start, *directives_len, *name, true),
                _ => continue,
            };
            if directives_len == 0 {
                continue;
            }
            let directives = self.rir.get_directives(directives_start, directives_len);
            let mut attached = Vec::new();
            for d in &directives {
                if d.name == derive_dir_sym {
                    if d.args.len() != 1 {
                        return Err(CompileError::new(
                            ErrorKind::DeriveNotADerive {
                                name: "<wrong arg count>".to_string(),
                                found: format!("{} arguments", d.args.len()),
                            },
                            d.span,
                        ));
                    }
                    attached.push((d.args[0], d.span));
                }
            }
            if !attached.is_empty() {
                raw.push(RawAttach {
                    host_name: name,
                    host_is_enum,
                    host_span: inst.span,
                    derive_names: attached,
                });
            }
        }

        // Resolve each binding.
        for r in raw {
            for (derive_name, dir_span) in r.derive_names {
                // `@derive(Copy)` is compiler-recognized (ADR-0059): the
                // field-Copy validation already runs via the `is_copy`
                // bookkeeping, and there is no derive item to look up. Skip
                // the regular resolution path.
                if self.is_compiler_derive(derive_name) {
                    continue;
                }

                let name_str = self.interner.resolve(&derive_name).to_string();

                // Resolution order: a `derive` item, else categorize what
                // we actually found for a clearer diagnostic.
                if !self.derives.contains_key(&derive_name) {
                    let found = if self.structs.contains_key(&derive_name) {
                        "struct"
                    } else if self.enums.contains_key(&derive_name) {
                        "enum"
                    } else if self.interfaces.contains_key(&derive_name) {
                        "interface"
                    } else if self.functions.contains_key(&derive_name) {
                        "function"
                    } else {
                        "unknown name"
                    };
                    return Err(CompileError::new(
                        ErrorKind::DeriveNotADerive {
                            name: name_str,
                            found: found.to_string(),
                        },
                        dir_span,
                    ));
                }

                self.derive_bindings.push(DeriveBinding {
                    host_name: r.host_name,
                    host_is_enum: r.host_is_enum,
                    derive_name,
                    host_span: r.host_span,
                    directive_span: dir_span,
                });
            }
        }

        Ok(())
    }

    /// ADR-0059 / ADR-0079: a name was previously recognized as a
    /// compiler-handled derive (`Copy` was the only such name).
    /// ADR-0080 retired `Copy` from the interface registry, so this
    /// hook always returns `false` and `@derive(...)` resolution
    /// flows entirely through the standard derive-resolution path —
    /// `@derive(Copy)` falls through the existing "unknown interface"
    /// diagnostic, exactly as the ADR specified.
    fn is_compiler_derive(&self, _name: Spur) -> bool {
        false
    }

    /// ADR-0058 phases 1 + 2: gate `derive` items behind the
    /// `comptime_derives` preview feature, register each into
    /// `Sema::derives` for later expansion, and reject ill-formed bodies
    /// (duplicate names, name collisions with structs/enums/interfaces,
    /// duplicate methods within a single derive, and direct `self.field`
    /// projection — the host type's structure isn't known at
    /// derive-definition time).
    pub(crate) fn validate_derive_decls(&mut self) -> CompileResult<()> {
        use super::DeriveInfo;
        use crate::sema::info::DeriveMethod;

        // Snapshot: copy out enough RIR data so we can mutate `self.derives`
        // and emit diagnostics without holding shared borrows on `self.rir`.
        struct RawDerive {
            name: Spur,
            decl_ref: gruel_rir::InstRef,
            span: Span,
            methods_start: u32,
            methods_len: u32,
        }
        let mut raw: Vec<RawDerive> = Vec::new();
        for (inst_ref, inst) in self.rir.iter() {
            if let InstData::DeriveDecl {
                name,
                methods_start,
                methods_len,
            } = &inst.data
            {
                raw.push(RawDerive {
                    name: *name,
                    decl_ref: inst_ref,
                    span: inst.span,
                    methods_start: *methods_start,
                    methods_len: *methods_len,
                });
            }
        }

        for d in raw {
            let name_str = self.interner.resolve(&d.name).to_string();

            // ADR-0079: a derive may share its name with an interface so the
            // prelude can ship `derive Clone {...}` alongside
            // `interface Clone {...}` — the derive provides an implementation
            // for that interface. Other collisions (struct, enum, another
            // derive) remain rejected.
            if self.derives.contains_key(&d.name)
                || self.structs.contains_key(&d.name)
                || self.enums.contains_key(&d.name)
            {
                return Err(CompileError::new(
                    ErrorKind::DuplicateTypeDefinition {
                        type_name: format!("derive `{}`", name_str),
                    },
                    d.span,
                ));
            }

            let method_refs = self.rir.get_inst_refs(d.methods_start, d.methods_len);
            let mut seen: HashSet<Spur> = HashSet::default();
            let mut methods: Vec<DeriveMethod> = Vec::with_capacity(method_refs.len());
            for method_ref in method_refs {
                let m = self.rir.get(method_ref);
                let InstData::FnDecl {
                    name: method_name,
                    has_self,
                    ..
                } = &m.data
                else {
                    // The grammar only admits method declarations inside a
                    // derive body, but if RIR ever produces something else
                    // we surface a clear diagnostic instead of panicking.
                    return Err(CompileError::new(
                        ErrorKind::InternalError(format!(
                            "non-method instruction inside `derive {}` body",
                            name_str
                        )),
                        m.span,
                    ));
                };

                if !seen.insert(*method_name) {
                    return Err(CompileError::new(
                        ErrorKind::DuplicateMethod {
                            type_name: format!("derive `{}`", name_str),
                            method_name: self.interner.resolve(method_name).to_string(),
                        },
                        m.span,
                    ));
                }

                self.reject_direct_self_projection(*method_name, &name_str, method_ref)?;

                methods.push(DeriveMethod {
                    name: *method_name,
                    has_self: *has_self,
                    method_ref,
                    span: m.span,
                });
            }

            self.derives.insert(
                d.name,
                DeriveInfo {
                    name: d.name,
                    decl_ref: d.decl_ref,
                    span: d.span,
                    methods,
                },
            );
        }

        Ok(())
    }

    /// Walk the RIR range `[body_start, decl)` for the method whose `FnDecl`
    /// is at `method_ref`, and reject any `FieldGet { base: VarRef "self" }`.
    /// The host's structure isn't known at derive-definition time, so direct
    /// projection (`self.x`) is illegal — users must go through
    /// `@field(self, "x")` (ADR-0058).
    fn reject_direct_self_projection(
        &self,
        method_name: Spur,
        derive_name: &str,
        method_ref: gruel_rir::InstRef,
    ) -> CompileResult<()> {
        let self_sym = match self.interner.get("self") {
            Some(s) => s,
            // No `self` symbol exists yet, so no method can reference it.
            None => return Ok(()),
        };

        // Find this method's function span so we can iterate just its body.
        let span = match self
            .rir
            .function_spans()
            .iter()
            .find(|s| s.decl == method_ref)
        {
            Some(s) => s,
            None => return Ok(()),
        };

        let view = self.rir.function_view(span);
        for (_, inst) in view.iter() {
            if let InstData::FieldGet { base, .. } = &inst.data {
                let base_inst = self.rir.get(*base);
                if let InstData::VarRef { name } = &base_inst.data
                    && *name == self_sym
                {
                    return Err(CompileError::new(
                        ErrorKind::DeriveDirectFieldAccess {
                            derive_name: derive_name.to_string(),
                            method_name: self.interner.resolve(&method_name).to_string(),
                        },
                        inst.span,
                    ));
                }
            }
        }
        Ok(())
    }

    /// Resolve enum variant field types. Must run after all type names are registered
    /// so that field types can reference other enums/structs.
    pub(crate) fn resolve_enum_variant_fields(&mut self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            if let InstData::EnumDecl {
                name,
                variants_start,
                variants_len,
                ..
            } = &inst.data
            {
                let enum_id = match self.enums.get(name) {
                    Some(id) => *id,
                    None => continue, // not registered (shouldn't happen)
                };

                let raw_variants = self
                    .rir
                    .get_enum_variant_decls(*variants_start, *variants_len);
                let has_data = raw_variants.iter().any(|(_, fields, _)| !fields.is_empty());
                if !has_data {
                    continue; // unit-only enum, no field types to resolve
                }

                let mut resolved_variants = Vec::with_capacity(raw_variants.len());
                for (vname, field_type_spurs, field_name_spurs) in &raw_variants {
                    let mut resolved_fields = Vec::with_capacity(field_type_spurs.len());
                    for field_ty_spur in field_type_spurs {
                        let field_ty = self.resolve_type(*field_ty_spur, inst.span)?;
                        resolved_fields.push(field_ty);
                    }
                    let field_names: Vec<String> = field_name_spurs
                        .iter()
                        .map(|n| self.interner.resolve(n).to_string())
                        .collect();
                    resolved_variants.push(EnumVariantDef {
                        name: self.interner.resolve(vname).to_string(),
                        fields: resolved_fields,
                        field_names,
                    });
                }

                let mut enum_def = self.type_pool.enum_def(enum_id);
                enum_def.variants = resolved_variants;
                self.type_pool.update_enum_def(enum_id, enum_def);
            }
        }
        Ok(())
    }

    /// ADR-0080 Phase 3: posture-consistency walker.
    ///
    /// Runs after `resolve_struct_fields` and `resolve_enum_variant_fields`
    /// have populated every field/variant `Type`, but before the rest of
    /// `resolve_remaining_declarations`. For every named struct or enum
    /// declaration, it classifies each member's posture (Copy / Affine /
    /// Linear) and folds the result into the propagated posture, then
    /// compares against the declared posture from the keyword:
    ///
    /// | Declared    | Rule                                   |
    /// |-------------|----------------------------------------|
    /// | `copy`      | every member must be Copy              |
    /// | (unmarked)  | no member may be Linear                |
    /// | `linear`    | (no constraint — linear holds anything)|
    ///
    /// On mismatch the error span is the host declaration's span and the
    /// message names the offending member's type and posture (richer
    /// per-field spans land when struct/enum field positions become
    /// addressable from `StructDef` / `EnumDef`).
    ///
    /// The walker is one function rather than a struct-validator + enum-
    /// validator pair because the only difference between the two cases is
    /// how members are enumerated; the posture-folding logic is identical.
    pub(crate) fn validate_posture_consistency(&self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            match &inst.data {
                InstData::StructDecl { name, .. } => {
                    let Some(&struct_id) = self.structs.get(name) else {
                        continue;
                    };
                    let def = self.type_pool.struct_def(struct_id);
                    let declared = if def.is_copy {
                        DeclaredPosture::Copy
                    } else if def.is_linear {
                        DeclaredPosture::Linear
                    } else {
                        DeclaredPosture::Affine
                    };
                    if declared == DeclaredPosture::Linear {
                        continue;
                    }
                    let host_name = def.name.clone();
                    for field in &def.fields {
                        let posture = self.classify_posture(field.ty);
                        if let Some(err) = check_posture_against_declared(
                            "struct",
                            host_name.as_str(),
                            declared,
                            "field",
                            &field.name,
                            self.format_type_name(field.ty).as_str(),
                            posture,
                            inst.span,
                        ) {
                            return Err(err);
                        }
                    }
                }
                InstData::EnumDecl { name, .. } => {
                    let Some(&enum_id) = self.enums.get(name) else {
                        continue;
                    };
                    let def = self.type_pool.enum_def(enum_id);
                    let declared = if def.is_copy {
                        DeclaredPosture::Copy
                    } else if def.is_linear {
                        DeclaredPosture::Linear
                    } else {
                        DeclaredPosture::Affine
                    };
                    if declared == DeclaredPosture::Linear {
                        continue;
                    }
                    let host_name = def.name.clone();
                    for variant in &def.variants {
                        for (i, field_ty) in variant.fields.iter().enumerate() {
                            let member_name = if let Some(name) = variant.field_names.get(i) {
                                format!("{}::{}.{}", host_name, variant.name, name)
                            } else {
                                format!("{}::{}.{}", host_name, variant.name, i)
                            };
                            let posture = self.classify_posture(*field_ty);
                            if let Some(err) = check_posture_against_declared(
                                "enum",
                                host_name.as_str(),
                                declared,
                                "variant field",
                                &member_name,
                                self.format_type_name(*field_ty).as_str(),
                                posture,
                                inst.span,
                            ) {
                                return Err(err);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Classify a type's ownership posture (ADR-0080).
    fn classify_posture(&self, ty: crate::types::Type) -> MemberPosture {
        if self.is_type_linear(ty) {
            MemberPosture::Linear
        } else if self.is_type_copy(ty) {
            MemberPosture::Copy
        } else {
            MemberPosture::Affine
        }
    }

    // ADR-0080 helper end.

    /// Resolve struct field types. Must run before @derive(Copy) validation.
    pub(crate) fn resolve_struct_fields(&mut self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            if let InstData::StructDecl {
                name,
                fields_start,
                fields_len,
                ..
            } = &inst.data
            {
                let name_str = self.interner.resolve(name).to_string();
                // Verify the struct exists in our lookup table
                if !self.structs.contains_key(name) {
                    return Err(CompileError::new(
                        ErrorKind::InternalError(
                            ice!(
                                "struct not found in struct map",
                                phase: "sema/declarations",
                                details: {
                                    "struct_name" => name_str.to_string()
                                }
                            )
                            .to_string(),
                        ),
                        inst.span,
                    ));
                }

                // Get the struct ID from the lookup table
                let struct_id = *self.structs.get(name).ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::InternalError(
                            ice!(
                                "struct not found in structs map",
                                phase: "sema/declarations",
                                details: {
                                    "struct_name" => name_str.to_string()
                                }
                            )
                            .to_string(),
                        ),
                        inst.span,
                    )
                })?;

                let struct_name = name_str.clone();
                let fields = self
                    .rir
                    .get_field_decls_with_vis(*fields_start, *fields_len);

                // Check for duplicate field names
                let mut seen_fields: HashSet<Spur> = HashSet::default();
                for (field_name, _, _) in &fields {
                    if !seen_fields.insert(*field_name) {
                        let field_name_str = self.interner.resolve(field_name).to_string();
                        return Err(CompileError::new(
                            ErrorKind::DuplicateField {
                                struct_name,
                                field_name: field_name_str,
                            },
                            inst.span,
                        ));
                    }
                }

                // Resolve field types
                let mut resolved_fields = Vec::new();
                for (field_name, field_type, is_pub) in &fields {
                    let field_ty = self.resolve_type(*field_type, inst.span)?;
                    resolved_fields.push(StructField {
                        name: self.interner.resolve(field_name).to_string(),
                        ty: field_ty,

                        is_pub: *is_pub,
                    });
                }

                // Update the struct definition in the pool with resolved fields
                let mut struct_def = self.type_pool.struct_def(struct_id);
                struct_def.fields = resolved_fields;
                self.type_pool.update_struct_def(struct_id, struct_def);
            }
        }
        Ok(())
    }

    /// Resolve @derive(Copy) validation, destructors, functions, and methods.
    pub(crate) fn resolve_remaining_declarations(&mut self) -> CompileResult<()> {
        // Collect all method InstRefs that should NOT be processed as
        // top-level functions:
        //
        // - Anonymous struct/enum methods are registered later during
        //   comptime evaluation with proper Self resolution.
        // - `derive` body methods (ADR-0058) are spliced into a host type
        //   at expansion time.
        // - Named struct/enum methods (including associated functions like
        //   `fn new() -> Self`) are collected via `collect_struct_methods` /
        //   `collect_enum_methods` with `current_self` set per ADR-0076.
        let mut anon_type_method_refs = rustc_hash::FxHashSet::default();
        for (_, inst) in self.rir.iter() {
            let (methods_start, methods_len) = match &inst.data {
                InstData::AnonStructType {
                    methods_start,
                    methods_len,
                    ..
                } => (*methods_start, *methods_len),
                InstData::AnonEnumType {
                    methods_start,
                    methods_len,
                    ..
                } => (*methods_start, *methods_len),
                InstData::DeriveDecl {
                    methods_start,
                    methods_len,
                    ..
                } => (*methods_start, *methods_len),
                InstData::StructDecl {
                    methods_start,
                    methods_len,
                    ..
                } => (*methods_start, *methods_len),
                InstData::EnumDecl {
                    methods_start,
                    methods_len,
                    ..
                } => (*methods_start, *methods_len),
                _ => continue,
            };
            let method_refs = self.rir.get_inst_refs(methods_start, methods_len);
            for method_ref in method_refs {
                anon_type_method_refs.insert(method_ref);
            }
        }

        // First pass: collect all declarations and validate @derive(Copy) structs
        for (inst_ref, inst) in self.rir.iter() {
            match &inst.data {
                InstData::StructDecl {
                    directives_start,
                    directives_len,
                    name,
                    methods_start,
                    methods_len,
                    ..
                } => {
                    // ADR-0079: `validate_copy_struct` and
                    // `validate_clone_struct` are both retired. The
                    // prelude `derive Copy` / `derive Clone` bodies
                    // express the field-Copy / field-Clone
                    // invariants in Gruel via
                    // `comptime_unroll for f in @type_info(Self).fields`
                    // + `comptime if (!@implements(f.field_type, …))`
                    // + `@compile_error`. Linearity is enforced by
                    // the structural Copy/Clone conformance checks.
                    let _ = (*directives_start, *directives_len);
                    // Collect methods defined inline in the struct
                    self.collect_struct_methods(*name, *methods_start, *methods_len, inst.span)?;
                }

                InstData::EnumDecl {
                    name,
                    methods_start,
                    methods_len,
                    ..
                } => {
                    // Collect methods defined inline in the enum (ADR-0053).
                    self.collect_enum_methods(*name, *methods_start, *methods_len, inst.span)?;
                }

                InstData::DropFnDecl { type_name, .. } => {
                    self.collect_destructor(*type_name, inst.span)?;
                }

                InstData::FnDecl {
                    is_pub,
                    is_unchecked,
                    name,
                    params_start,
                    params_len,
                    return_type,
                    body,
                    has_self,
                    ..
                } => {
                    // Skip methods (has_self = true) - these are handled elsewhere:
                    // - Named struct methods are collected via ImplDecl
                    if *has_self {
                        continue;
                    }

                    // Skip ALL methods from anonymous types (structs and enums)
                    // These are registered during comptime evaluation with proper Self type context
                    if anon_type_method_refs.contains(&inst_ref) {
                        continue;
                    }
                    self.collect_function_signature(
                        *name,
                        (*params_start, *params_len),
                        *return_type,
                        *body,
                        inst.span,
                        FnFlags {
                            is_pub: *is_pub,
                            is_unchecked: *is_unchecked,
                        },
                    )?;
                }

                InstData::ConstDecl {
                    is_pub, name, init, ..
                } => {
                    self.collect_const_declaration(*name, *is_pub, *init, inst.span)?;
                }

                _ => {}
            }
        }

        Ok(())
    }

    /// Collect a destructor definition and register it with its struct.
    fn collect_destructor(&mut self, type_name: Spur, span: Span) -> CompileResult<()> {
        let type_name_str = self.interner.resolve(&type_name).to_string();

        // Verify the struct exists
        if !self.structs.contains_key(&type_name) {
            return Err(CompileError::new(
                ErrorKind::DestructorUnknownType {
                    type_name: type_name_str,
                },
                span,
            ));
        }

        // Get the struct ID from the lookup table
        let struct_id = *self.structs.get(&type_name).ok_or_else(|| {
            CompileError::new(
                ErrorKind::InternalError(
                    ice!(
                        "struct not found during destructor collection",
                        phase: "sema/declarations",
                        details: {
                            "struct_name" => type_name_str.to_string()
                        }
                    )
                    .to_string(),
                ),
                span,
            )
        })?;

        let mut struct_def = self.type_pool.struct_def(struct_id);
        if struct_def.destructor.is_some() {
            return Err(CompileError::new(
                ErrorKind::DuplicateDestructor {
                    type_name: type_name_str,
                },
                span,
            ));
        }

        let destructor_name = format!("{}.__drop", type_name_str);
        struct_def.destructor = Some(destructor_name);
        self.type_pool.update_struct_def(struct_id, struct_def);
        Ok(())
    }

    /// Collect a function signature for forward reference.
    fn collect_function_signature(
        &mut self,
        name: Spur,
        (params_start, params_len): (u32, u32),
        return_type_sym: Spur,
        body: InstRef,
        span: Span,
        flags: FnFlags,
    ) -> CompileResult<()> {
        let FnFlags {
            is_pub,
            is_unchecked,
        } = flags;
        let params = self.rir.get_params(params_start, params_len);

        let param_names: Vec<Spur> = params.iter().map(|p| p.name).collect();

        // Check if this function has any comptime TYPE parameters (not value parameters).
        // A type parameter is a comptime param whose declared type is either:
        // - `type` (unbounded): `comptime T: type`
        // - a registered interface name (ADR-0056): `comptime T: SomeInterface`
        //
        // Both cases erase the parameter at codegen and substitute the
        // concrete type at specialization. Interface-bounded parameters
        // additionally trigger a conformance check at the call site.
        let type_sym = self.interner.get_or_intern("type");
        // Pre-compute per-param flags so the closure doesn't need to borrow
        // `self` (which conflicts with the resolve_type call below).
        let is_type_param: Vec<bool> = params
            .iter()
            .map(|p| p.is_comptime && (p.ty == type_sym || self.interfaces.contains_key(&p.ty)))
            .collect();
        let is_generic = is_type_param.iter().any(|b| *b);

        // Collect type parameter names.
        let type_param_names: Vec<Spur> = params
            .iter()
            .zip(is_type_param.iter())
            .filter_map(|(p, &b)| if b { Some(p.name) } else { None })
            .collect();

        // Record any interface bounds for use at specialization time.
        for p in params.iter() {
            if p.is_comptime
                && let Some(iid) = self.interfaces.get(&p.ty).copied()
            {
                self.comptime_interface_bounds.insert((name, p.name), iid);
            }
        }

        // For generic functions, we defer type resolution of type parameters until specialization.
        // We use Type::COMPTIME_TYPE as a placeholder for comptime type parameters.
        // ADR-0076 Phase 2: also collect the (possibly normalized) param mode
        // returned by `resolve_param_type` so `Ref(I)` / `MutRef(I)` lower to
        // the same `(Interface, Borrow|Inout)` shape as legacy `borrow t: I`
        // / `inout t: I`.
        let mut param_types: Vec<Type> = Vec::with_capacity(params.len());
        let mut param_modes: Vec<RirParamMode> = Vec::with_capacity(params.len());
        for (p, &is_tp) in params.iter().zip(is_type_param.iter()) {
            let (ty, mode) = if is_tp || type_param_names.contains(&p.ty) {
                // Comptime type parameter, or a parameter whose declared type
                // is a type parameter (`x: T`). The concrete type is resolved
                // at specialization; mode is taken verbatim from RIR.
                (Type::COMPTIME_TYPE, p.mode)
            } else {
                self.resolve_param_type(p.ty, p.mode, span)?
            };
            param_types.push(ty);
            param_modes.push(mode);
        }
        let param_comptime: Vec<bool> = params.iter().map(|p| p.is_comptime).collect();

        // For generic functions, we can't resolve the return type yet if it references
        // a type parameter. For now, check if it matches any type parameter name.
        let ret_type = if type_param_names.contains(&return_type_sym) {
            // Return type is a type parameter - use placeholder
            Type::COMPTIME_TYPE
        } else {
            self.resolve_type(return_type_sym, span)?
        };

        // Allocate parameter data in the arena
        let params_range =
            self.param_arena
                .alloc(param_names, param_types, param_modes, param_comptime);

        self.functions.insert(
            name,
            FunctionInfo {
                params: params_range,
                return_type: ret_type,
                return_type_sym,
                body,
                span,
                is_generic,
                is_pub,
                is_unchecked,
                file_id: span.file_id,
                canonical_name: None,
            },
        );
        Ok(())
    }

    /// Collect methods defined inline in a struct.
    fn collect_struct_methods(
        &mut self,
        type_name: Spur,
        methods_start: u32,
        methods_len: u32,
        span: Span,
    ) -> CompileResult<()> {
        let struct_id = match self.structs.get(&type_name) {
            Some(id) => *id,
            None => {
                let type_name_str = self.interner.resolve(&type_name).to_string();
                return Err(CompileError::new(
                    ErrorKind::UnknownType(type_name_str),
                    span,
                ));
            }
        };
        let struct_type = Type::new_struct(struct_id);
        let drop_name_sym = self.interner.get_or_intern("drop");

        // ADR-0076: bind `Self` to the host struct while resolving method
        // signatures. Errors abort the whole analysis pass so a leak is
        // harmless; on the success path we restore the previous binding.
        let saved_self = self.current_self.replace(struct_type);

        let methods = self.rir.get_inst_refs(methods_start, methods_len);
        for method_ref in methods {
            let method_inst = self.rir.get(method_ref);
            if let InstData::FnDecl {
                name: method_name,
                is_pub: method_is_pub,
                is_unchecked,
                params_start,
                params_len,
                return_type,
                body,
                has_self,
                receiver_mode,
                ..
            } = &method_inst.data
            {
                let receiver = decode_receiver_mode(*receiver_mode);
                // ADR-0053: a method named `drop` is the struct's destructor,
                // not a regular method. Route it through the destructor slot.
                if *method_name == drop_name_sym {
                    self.register_inline_struct_drop(
                        type_name,
                        struct_id,
                        DropSignature {
                            has_self: *has_self,
                            params_len: *params_len,
                            return_type: *return_type,
                            body: *body,
                            span: method_inst.span,
                        },
                    )?;
                    continue;
                }

                // Use StructId in key to support anonymous struct methods
                let key = (struct_id, *method_name);
                if self.methods.contains_key(&key) {
                    let type_name_str = self.interner.resolve(&type_name).to_string();
                    let method_name_str = self.interner.resolve(method_name).to_string();
                    return Err(CompileError::new(
                        ErrorKind::DuplicateMethod {
                            type_name: type_name_str,
                            method_name: method_name_str,
                        },
                        method_inst.span,
                    ));
                }

                let params = self.rir.get_params(*params_start, *params_len);
                let param_names: Vec<Spur> = params.iter().map(|p| p.name).collect();
                let param_comptime: Vec<bool> = params.iter().map(|p| p.is_comptime).collect();

                // Detect method-level comptime type parameters (e.g.,
                // `fn apply(self, comptime F: type, f: F) -> T`). When
                // present, the method is generic at the method level — its
                // param types that reference those names (`f: F`) and its
                // return type cannot be fully resolved until the method is
                // specialized at a call site. Mirrors the top-level
                // generic-function treatment above.
                //
                // ADR-0056: an interface-bounded comptime parameter
                // (`comptime T: SomeInterface`) is also a type parameter; we
                // record the bound for the conformance check at the call
                // site. The method-key is encoded as "StructName.method" to
                // share the bound side-table with top-level functions.
                let type_sym = self.interner.get_or_intern("type");
                let method_type_param_names: Vec<Spur> = params
                    .iter()
                    .filter(|p| {
                        p.is_comptime && (p.ty == type_sym || self.interfaces.contains_key(&p.ty))
                    })
                    .map(|p| p.name)
                    .collect();
                let is_method_generic = !method_type_param_names.is_empty();

                // Record interface bounds for this method.
                if !method_type_param_names.is_empty() {
                    let owner_str = format!(
                        "{}.{}",
                        self.interner.resolve(&type_name),
                        self.interner.resolve(method_name)
                    );
                    let owner = self.interner.get_or_intern(&owner_str);
                    for p in params.iter() {
                        if p.is_comptime
                            && let Some(iid) = self.interfaces.get(&p.ty).copied()
                        {
                            self.comptime_interface_bounds.insert((owner, p.name), iid);
                        }
                    }
                }

                // Helper: does a type symbol mention any of our method-level
                // type param names? Covers bare `T`, compound `[T; N]`,
                // `ptr const T`, etc. — check via the comptime-substitution
                // resolver using a sentinel substitution.
                let references_method_type_param = |ty_sym: Spur, sema: &mut Self| -> bool {
                    if method_type_param_names.contains(&ty_sym) {
                        return true;
                    }
                    let subst: rustc_hash::FxHashMap<Spur, Type> = method_type_param_names
                        .iter()
                        .map(|&n| (n, Type::I32))
                        .collect();
                    let with_subst = sema.resolve_type_for_comptime_with_subst(ty_sym, &subst);
                    let without_subst =
                        sema.resolve_type_for_comptime_with_subst(ty_sym, &HashMap::default());
                    with_subst.is_some() && without_subst.is_none()
                };

                // ADR-0076 Phase 2: collect normalized modes alongside types
                // so `Ref(I)` / `MutRef(I)` lower like the legacy keyword form.
                let mut param_types: Vec<Type> = Vec::with_capacity(params.len());
                let mut param_modes: Vec<RirParamMode> = Vec::with_capacity(params.len());
                for p in &params {
                    let (ty, mode) = if (p.is_comptime && p.ty == type_sym)
                        || references_method_type_param(p.ty, self)
                    {
                        (Type::COMPTIME_TYPE, p.mode)
                    } else {
                        self.resolve_param_type(p.ty, p.mode, method_inst.span)?
                    };
                    param_types.push(ty);
                    param_modes.push(mode);
                }
                let ret_type = if references_method_type_param(*return_type, self) {
                    Type::COMPTIME_TYPE
                } else {
                    self.resolve_type(*return_type, method_inst.span)?
                };

                // Allocate method parameters in the arena, preserving mode
                // and comptime flags so specialization can pick them up.
                let param_range =
                    self.param_arena
                        .alloc(param_names, param_types, param_modes, param_comptime);

                self.methods.insert(
                    key,
                    MethodInfo {
                        struct_type,
                        has_self: *has_self,
                        receiver,
                        params: param_range,
                        return_type: ret_type,
                        body: *body,
                        span: method_inst.span,
                        is_unchecked: *is_unchecked,
                        is_generic: is_method_generic,
                        return_type_sym: *return_type,
                        is_pub: *method_is_pub,
                        file_id: method_inst.span.file_id,
                    },
                );
            }
        }
        self.current_self = saved_self;
        Ok(())
    }

    /// Register an inline `fn drop(self)` as a struct's destructor.
    ///
    /// Validates the signature (must be exactly `fn drop(self)`, no extra
    /// params, returns unit) and the type's copy/linear status (a destructor
    /// is illegal on `@derive(Copy)` and `linear` types per ADR-0053).
    fn register_inline_struct_drop(
        &mut self,
        type_name: Spur,
        struct_id: StructId,
        sig: DropSignature,
    ) -> CompileResult<()> {
        let DropSignature {
            has_self,
            params_len,
            return_type,
            body,
            span,
        } = sig;
        let type_name_str = self.interner.resolve(&type_name).to_string();

        // Signature check: `fn drop(self)`, no extra params, no non-unit return.
        if !has_self {
            return Err(CompileError::new(
                ErrorKind::InvalidInlineDrop {
                    type_name: type_name_str.clone(),
                    reason: "must take `self` — found an associated function".into(),
                },
                span,
            ));
        }
        if params_len > 0 {
            return Err(CompileError::new(
                ErrorKind::InvalidInlineDrop {
                    type_name: type_name_str.clone(),
                    reason: "must take only `self` — extra parameters are not allowed".into(),
                },
                span,
            ));
        }
        let ret_str = self.interner.resolve(&return_type);
        if ret_str != "()" {
            return Err(CompileError::new(
                ErrorKind::InvalidInlineDrop {
                    type_name: type_name_str.clone(),
                    reason: format!("must return unit — found return type `{}`", ret_str),
                },
                span,
            ));
        }

        // Affine-only: `@derive(Copy)` structs cannot have destructors (double-free risk).
        // `linear` structs cannot either — they are never implicitly dropped, so
        // a destructor would be unreachable.
        let struct_def_snapshot = self.type_pool.struct_def(struct_id);
        if struct_def_snapshot.is_copy {
            return Err(CompileError::new(
                ErrorKind::InvalidInlineDrop {
                    type_name: type_name_str.clone(),
                    reason:
                        "`@derive(Copy)` types cannot declare `fn drop` (would double-free on copy)"
                            .into(),
                },
                span,
            ));
        }
        if struct_def_snapshot.is_linear {
            return Err(CompileError::new(
                ErrorKind::InvalidInlineDrop {
                    type_name: type_name_str.clone(),
                    reason: "`linear` types cannot declare `fn drop` (linear values are never implicitly dropped)".into(),
                },
                span,
            ));
        }

        // Only one destructor per type.
        let mut struct_def = struct_def_snapshot;
        if struct_def.destructor.is_some() {
            return Err(CompileError::new(
                ErrorKind::DuplicateDestructor {
                    type_name: type_name_str.clone(),
                },
                span,
            ));
        }
        let destructor_name = format!("{}.__drop", type_name_str);
        struct_def.destructor = Some(destructor_name);
        self.type_pool.update_struct_def(struct_id, struct_def);

        self.inline_struct_drops.insert(struct_id, (body, span));
        Ok(())
    }

    /// Register an inline `fn drop(self)` as an enum's destructor (ADR-0053 phase 3b).
    ///
    /// Mirrors `register_inline_struct_drop`. Validates the signature
    /// (must be `fn drop(self)` returning unit, only one per type) and
    /// stores the destructor metadata in `EnumDef.destructor`.
    fn register_inline_enum_drop(
        &mut self,
        type_name: Spur,
        enum_id: EnumId,
        sig: DropSignature,
    ) -> CompileResult<()> {
        let DropSignature {
            has_self,
            params_len,
            return_type,
            body,
            span,
        } = sig;
        let type_name_str = self.interner.resolve(&type_name).to_string();

        if !has_self {
            return Err(CompileError::new(
                ErrorKind::InvalidInlineDrop {
                    type_name: type_name_str,
                    reason: "must take `self` — found an associated function".into(),
                },
                span,
            ));
        }
        if params_len > 0 {
            return Err(CompileError::new(
                ErrorKind::InvalidInlineDrop {
                    type_name: type_name_str,
                    reason: "must take only `self` — extra parameters are not allowed".into(),
                },
                span,
            ));
        }
        let ret_str = self.interner.resolve(&return_type);
        if ret_str != "()" {
            return Err(CompileError::new(
                ErrorKind::InvalidInlineDrop {
                    type_name: type_name_str,
                    reason: format!("must return unit — found return type `{}`", ret_str),
                },
                span,
            ));
        }

        let mut enum_def = self.type_pool.enum_def(enum_id);
        if enum_def.destructor.is_some() {
            return Err(CompileError::new(
                ErrorKind::DuplicateDestructor {
                    type_name: type_name_str,
                },
                span,
            ));
        }
        let destructor_name = format!("{}.__drop", type_name_str);
        enum_def.destructor = Some(destructor_name);
        self.type_pool.update_enum_def(enum_id, enum_def);

        self.inline_enum_drops.insert(enum_id, (body, span));
        Ok(())
    }

    /// Collect methods defined inline in a named enum (ADR-0053).
    ///
    /// Mirrors `collect_struct_methods`: registers each method against the
    /// enum's `EnumId` in `self.enum_methods`, which the method-resolution
    /// machinery already consults for anonymous enums (ADR-0039).
    fn collect_enum_methods(
        &mut self,
        type_name: Spur,
        methods_start: u32,
        methods_len: u32,
        span: Span,
    ) -> CompileResult<()> {
        if methods_len == 0 {
            return Ok(());
        }
        let enum_id = match self.enums.get(&type_name) {
            Some(id) => *id,
            None => {
                let type_name_str = self.interner.resolve(&type_name).to_string();
                return Err(CompileError::new(
                    ErrorKind::UnknownType(type_name_str),
                    span,
                ));
            }
        };
        let enum_type = Type::new_enum(enum_id);
        let drop_name_sym = self.interner.get_or_intern("drop");

        // ADR-0076: bind `Self` to the host enum while resolving method
        // signatures.
        let saved_self = self.current_self.replace(enum_type);

        let methods = self.rir.get_inst_refs(methods_start, methods_len);
        for method_ref in methods {
            let method_inst = self.rir.get(method_ref);
            if let InstData::FnDecl {
                name: method_name,
                is_pub: method_is_pub,
                is_unchecked,
                params_start,
                params_len,
                return_type,
                body,
                has_self,
                receiver_mode,
                ..
            } = &method_inst.data
            {
                let receiver = decode_receiver_mode(*receiver_mode);
                // ADR-0053 phase 3b: a method named `drop` is the enum's destructor,
                // not a regular method. Route it through the per-enum destructor slot.
                if *method_name == drop_name_sym {
                    self.register_inline_enum_drop(
                        type_name,
                        enum_id,
                        DropSignature {
                            has_self: *has_self,
                            params_len: *params_len,
                            return_type: *return_type,
                            body: *body,
                            span: method_inst.span,
                        },
                    )?;
                    continue;
                }
                let key = (enum_id, *method_name);
                if self.enum_methods.contains_key(&key) {
                    let type_name_str = self.interner.resolve(&type_name).to_string();
                    let method_name_str = self.interner.resolve(method_name).to_string();
                    return Err(CompileError::new(
                        ErrorKind::DuplicateMethod {
                            type_name: type_name_str,
                            method_name: method_name_str,
                        },
                        method_inst.span,
                    ));
                }

                let params = self.rir.get_params(*params_start, *params_len);
                let param_names: Vec<Spur> = params.iter().map(|p| p.name).collect();
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| self.resolve_type(p.ty, method_inst.span))
                    .collect::<CompileResult<Vec<_>>>()?;
                let ret_type = self.resolve_type(*return_type, method_inst.span)?;

                let param_range = self
                    .param_arena
                    .alloc_method(param_names.into_iter(), param_types.into_iter());

                self.enum_methods.insert(
                    key,
                    MethodInfo {
                        struct_type: enum_type,
                        has_self: *has_self,
                        receiver,
                        params: param_range,
                        return_type: ret_type,
                        body: *body,
                        span: method_inst.span,
                        is_unchecked: *is_unchecked,
                        // Enum methods do not yet support method-level
                        // comptime type params (ADR-0055 defers that path).
                        is_generic: false,
                        return_type_sym: *return_type,
                        is_pub: *method_is_pub,
                        file_id: method_inst.span.file_id,
                    },
                );
            }
        }
        self.current_self = saved_self;
        Ok(())
    }

    /// Collect a constant declaration.
    ///
    /// Constants are compile-time values. In the module system, they're primarily
    /// used for re-exports:
    /// ```gruel
    /// pub const strings = @import("utils/strings.gruel");
    /// ```
    ///
    /// When the initializer is an `@import(...)`, we evaluate it at compile time
    /// to resolve the module and register it in the module registry. This enables
    /// subsequent member access via `const_name.function()` syntax.
    fn collect_const_declaration(
        &mut self,
        name: Spur,
        is_pub: bool,
        init: InstRef,
        span: Span,
    ) -> CompileResult<()> {
        // ADR-0078: detect item-level re-exports first. `pub const X = mod.Y`
        // makes X an alias for `mod.Y`, registering it in the appropriate
        // name table (functions/structs/enums/interfaces) so call sites
        // can use X transparently. The fallback path below handles
        // `pub const X = @import("...")` (whole-module re-export) and
        // primitive constants.
        if self.try_collect_reexport(name, is_pub, init, span)? {
            return Ok(());
        }

        let name_str = self.interner.resolve(&name).to_string();

        // Check for duplicate constant names
        if self.constants.contains_key(&name) {
            return Err(CompileError::new(
                ErrorKind::DuplicateConstant {
                    name: name_str,
                    kind: "constant".to_string(),
                },
                span,
            ));
        }

        // Check for collision with function names
        if self.functions.contains_key(&name) {
            return Err(CompileError::new(
                ErrorKind::DuplicateConstant {
                    name: name_str.clone(),
                    kind: "constant (conflicts with function)".to_string(),
                },
                span,
            ));
        }

        // Evaluate the initializer at compile time to determine the constant type.
        let const_type = self.evaluate_const_init(init, span)?;

        self.constants.insert(
            name,
            ConstInfo {
                is_pub,
                ty: const_type,
                init,
                span,
            },
        );

        Ok(())
    }

    /// ADR-0078: Try to handle an item-level re-export of the form
    /// `pub const X = some_module_const.Y`. Returns `Ok(true)` if the
    /// constant was recognized as a re-export and registered as an alias;
    /// `Ok(false)` if this isn't a re-export and the regular const path
    /// should run; `Err(...)` if it looks like a re-export but the item
    /// can't be resolved.
    fn try_collect_reexport(
        &mut self,
        name: Spur,
        is_pub: bool,
        init: InstRef,
        span: Span,
    ) -> CompileResult<bool> {
        let inst = self.rir.get(init);
        let InstData::FieldGet { base, field } = &inst.data else {
            return Ok(false);
        };
        let field = *field;

        // The base must be a `VarRef` to a const that holds a module.
        let base_inst = self.rir.get(*base);
        let InstData::VarRef { name: base_name } = &base_inst.data else {
            return Ok(false);
        };
        let module_id = match self.constants.get(base_name).and_then(|c| c.ty.as_module()) {
            Some(id) => id,
            None => return Ok(false),
        };
        let module_file_path = self.module_registry.get_def(module_id).file_path.clone();
        let same_name = name == field;

        // Look the field up across each item kind, restricted to items
        // declared in the imported module's file.

        // Function.
        if let Some(fn_info) = self.functions.get(&field).copied() {
            let fn_path = self
                .get_file_path(fn_info.file_id)
                .map(|s| s.to_string())
                .unwrap_or_default();
            if fn_path == module_file_path {
                if !same_name {
                    self.check_alias_collision(name, span)?;
                    // Resolve to the canonical (non-aliased) name so chains
                    // of re-exports collapse to the original function symbol.
                    let canonical = fn_info.canonical_name.unwrap_or(field);
                    let alias = FunctionInfo {
                        is_pub,
                        canonical_name: Some(canonical),
                        ..fn_info
                    };
                    self.functions.insert(name, alias);
                }
                return Ok(true);
            }
        }

        // Struct.
        if let Some(&struct_id) = self.structs.get(&field) {
            let s_path = self
                .get_file_path(self.type_pool.struct_def(struct_id).file_id)
                .map(|s| s.to_string())
                .unwrap_or_default();
            if s_path == module_file_path {
                if !same_name {
                    self.check_alias_collision(name, span)?;
                    self.structs.insert(name, struct_id);
                }
                return Ok(true);
            }
        }

        // Enum.
        if let Some(&enum_id) = self.enums.get(&field) {
            let e_path = self
                .get_file_path(self.type_pool.enum_def(enum_id).file_id)
                .map(|s| s.to_string())
                .unwrap_or_default();
            if e_path == module_file_path {
                if !same_name {
                    self.check_alias_collision(name, span)?;
                    self.enums.insert(name, enum_id);
                }
                return Ok(true);
            }
        }

        // Interface.
        if let Some(&iface_id) = self.interfaces.get(&field) {
            let iface_def = &self.interface_defs[iface_id.0 as usize];
            let i_path = self
                .get_file_path(iface_def.file_id)
                .map(|s| s.to_string())
                .unwrap_or_default();
            if i_path == module_file_path {
                if !same_name {
                    self.check_alias_collision(name, span)?;
                    self.interfaces.insert(name, iface_id);
                }
                return Ok(true);
            }
        }

        // Field exists in the module's namespace but doesn't resolve to a
        // re-exportable item. Could be e.g. `mod.NotAnItem` — surface as a
        // standard "unknown module member" error.
        let module_def = self.module_registry.get_def(module_id);
        let module_name = module_def.import_path.clone();
        let field_str = self.interner.resolve(&field).to_string();
        Err(CompileError::new(
            ErrorKind::UnknownModuleMember {
                module_name,
                member_name: field_str,
            },
            span,
        ))
    }

    /// Collision check for a re-export alias. The alias name must not
    /// shadow an existing function/struct/enum/interface/constant.
    fn check_alias_collision(&self, name: Spur, span: Span) -> CompileResult<()> {
        let name_str = self.interner.resolve(&name).to_string();
        if self.functions.contains_key(&name)
            || self.structs.contains_key(&name)
            || self.enums.contains_key(&name)
            || self.interfaces.contains_key(&name)
            || self.constants.contains_key(&name)
        {
            return Err(CompileError::new(
                ErrorKind::DuplicateConstant {
                    name: name_str,
                    kind: "re-export alias".to_string(),
                },
                span,
            ));
        }
        Ok(())
    }

    /// Evaluate a constant initializer at compile time.
    ///
    /// Currently handles:
    /// - `@import("path")` - Returns Type::Module
    /// - Integer literals - Returns the integer type
    ///
    /// Future extensions (ADR-0025 comptime) will support:
    /// - Arithmetic on constants
    /// - comptime blocks
    /// - comptime function calls
    fn evaluate_const_init(&mut self, init: InstRef, span: Span) -> CompileResult<Type> {
        let init_inst = self.rir.get(init);

        match &init_inst.data {
            // @import("path") evaluates to Type::Module at compile time
            InstData::Intrinsic {
                name,
                args_start,
                args_len,
            } => {
                if *name == self.known.import {
                    // Validate exactly one argument
                    if *args_len != 1 {
                        return Err(CompileError::new(
                            ErrorKind::IntrinsicWrongArgCount {
                                name: "import".to_string(),
                                expected: 1,
                                found: *args_len as usize,
                            },
                            span,
                        ));
                    }

                    // Accept a string literal or a comptime_str expression.
                    let arg_refs = self.rir.get_inst_refs(*args_start, *args_len);
                    let import_path = self.resolve_import_path_arg(arg_refs[0])?;

                    // Resolve the import path to an absolute file path
                    let resolved_path = self.resolve_import_path(&import_path, span)?;

                    // Register the module in the registry
                    let (module_id, _is_new) = self
                        .module_registry
                        .get_or_create(import_path, resolved_path);

                    Ok(Type::new_module(module_id))
                } else {
                    // For other intrinsics in const context, we don't support them yet
                    let intrinsic_name = self.interner.resolve(name).to_string();
                    Err(CompileError::new(
                        ErrorKind::ConstExprNotSupported {
                            expr_kind: format!("@{} intrinsic", intrinsic_name),
                        },
                        span,
                    ))
                }
            }

            // Integer literals evaluate to i32 (the default integer type)
            // Note: RIR doesn't distinguish between integer types at this level;
            // type inference happens later. For now, we treat all integer consts as i32.
            InstData::IntConst(_) => Ok(Type::I32),

            // Boolean literals
            InstData::BoolConst(_) => Ok(Type::BOOL),

            // Unit literal
            InstData::UnitConst => Ok(Type::UNIT),

            // String literals
            InstData::StringConst(_) => {
                // String constants would need the String type
                // For now, we don't support them in const context
                Err(CompileError::new(
                    ErrorKind::ConstExprNotSupported {
                        expr_kind: "string literals".to_string(),
                    },
                    span,
                ))
            }

            // Other expressions are not yet supported in const context
            _ => Err(CompileError::new(
                ErrorKind::ConstExprNotSupported {
                    expr_kind: "this expression".to_string(),
                },
                span,
            )),
        }
    }
}

/// Build the trailing note for an unknown-directive error (ADR-0075).
///
/// Retired directives get a targeted retirement message pointing at the
/// ADR that took them out of service. Other names get an edit-distance
/// hint when there's a near-match in the recognized set; otherwise no
/// note is attached.
fn directive_diagnosis_note(name: &str) -> Option<String> {
    match name {
        "handle" => {
            return Some(
                "the `@handle` directive was retired in ADR-0075; \
                 conform to the `Handle` interface by defining \
                 `fn handle(self: Ref(Self)) -> Self` directly"
                    .to_string(),
            );
        }
        "copy" => {
            return Some(
                "the `@copy` directive was retired in ADR-0059; \
                 use `@derive(Copy)` instead"
                    .to_string(),
            );
        }
        _ => {}
    }

    const RECOGNIZED: &[&str] = &["allow", "derive"];
    RECOGNIZED
        .iter()
        .map(|cand| (cand, levenshtein_distance(name, cand)))
        .filter(|(_, d)| *d <= 2)
        .min_by_key(|(_, d)| *d)
        .map(|(cand, _)| format!("did you mean `@{cand}`?"))
}

/// Wagner-Fischer Levenshtein distance. Used by `directive_diagnosis_note`
/// to suggest near-matches for typo'd directive names. Both inputs are
/// short identifiers (~10 chars), so the O(n·m) cost is negligible and the
/// dependency-free implementation is preferable to pulling in `strsim`.
fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr: Vec<usize> = vec![0; m + 1];
    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (curr[j - 1] + 1).min(prev[j] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

#[cfg(test)]
mod directive_validation_tests {
    use super::{directive_diagnosis_note, levenshtein_distance};

    #[test]
    fn levenshtein_basics() {
        assert_eq!(levenshtein_distance("kitten", "sitting"), 3);
        assert_eq!(levenshtein_distance("allow", "allwo"), 2);
        assert_eq!(levenshtein_distance("derive", "dervie"), 2);
        assert_eq!(levenshtein_distance("derive", "derive"), 0);
        assert_eq!(levenshtein_distance("", "abc"), 3);
        assert_eq!(levenshtein_distance("abc", ""), 3);
    }

    #[test]
    fn note_for_typo_near_allow() {
        let note = directive_diagnosis_note("allwo").expect("near-match should suggest");
        assert!(note.contains("@allow"), "got: {note}");
    }

    #[test]
    fn note_for_typo_near_derive() {
        let note = directive_diagnosis_note("dervie").expect("near-match should suggest");
        assert!(note.contains("@derive"), "got: {note}");
    }

    #[test]
    fn note_for_far_typo_is_none() {
        assert!(directive_diagnosis_note("xyzzy").is_none());
    }

    #[test]
    fn note_for_retired_handle_mentions_adr_0075() {
        let note = directive_diagnosis_note("handle").expect("retirement note");
        assert!(note.contains("ADR-0075"), "got: {note}");
        assert!(note.contains("Handle"), "got: {note}");
    }

    #[test]
    fn note_for_retired_copy_mentions_adr_0059() {
        let note = directive_diagnosis_note("copy").expect("retirement note");
        assert!(note.contains("ADR-0059"), "got: {note}");
        assert!(note.contains("@derive(Copy)"), "got: {note}");
    }
}

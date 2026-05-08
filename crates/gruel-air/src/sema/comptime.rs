//! Compile-time evaluation for the semantic-analysis pass.
//!
//! Hosts the constant folder (`try_evaluate_const`) and the comptime
//! interpreter (`evaluate_comptime_*`) used during type-checking and
//! generic specialization. The methods here are extensions of `Sema`
//! split out from `analysis.rs` for readability — they reach into the
//! type/struct/enum tables but otherwise form a self-contained block.

use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

use gruel_intrinsics::IntrinsicId;
use gruel_rir::{InstData, InstRef, RirPattern};
use gruel_util::{BinOp, CompileError, CompileResult, ErrorKind, Span, UnaryOp};
use lasso::Spur;
use tracing::info_span;

use super::Sema;
use super::analysis::{AnonStructSpec, arch_variant_index, os_variant_index};
use super::context::{AnalysisContext, ComptimeHeapItem, ConstValue, ParamInfo};
use crate::types::{EnumId, EnumVariantDef, StructField, StructId, Type, TypeKind};

/// Span context for comptime evaluation: the outer eval site (used for
/// errors that bubble up through nested calls) and the current
/// instruction's span (used for diagnostics specific to this call).
#[derive(Clone, Copy)]
struct ComptimeSpans {
    outer: Span,
    inst: Span,
}

impl<'a> Sema<'a> {
    /// Try to evaluate an RIR expression as a compile-time constant.
    ///
    /// Returns `Some(value)` if the expression can be fully evaluated at compile time,
    /// or `None` if evaluation requires runtime information (e.g., variable values,
    /// function calls) or would cause overflow/panic.
    ///
    /// This is the foundation for compile-time bounds checking and can be extended
    /// for future `comptime` features.
    pub(crate) fn try_evaluate_const(&mut self, inst_ref: InstRef) -> Option<ConstValue> {
        let inst = self.rir.get(inst_ref);
        match &inst.data {
            // Integer literals
            InstData::IntConst(value) => i64::try_from(*value).ok().map(ConstValue::Integer),

            // Boolean literals
            InstData::BoolConst(value) => Some(ConstValue::Bool(*value)),

            InstData::Unary { op, operand } => {
                let v = self.try_evaluate_const(*operand)?;
                match (op, v) {
                    (UnaryOp::Neg, ConstValue::Integer(n)) => {
                        Some(ConstValue::Integer(n.wrapping_neg()))
                    }
                    (UnaryOp::Not, ConstValue::Bool(b)) => Some(ConstValue::Bool(!b)),
                    (UnaryOp::BitNot, ConstValue::Integer(n)) => Some(ConstValue::Integer(!n)),
                    _ => None,
                }
            }

            InstData::Bin { op, lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?;
                let r = self.try_evaluate_const(*rhs)?;
                match op {
                    BinOp::Add => Some(ConstValue::Integer(
                        l.as_integer()?.wrapping_add(r.as_integer()?),
                    )),
                    BinOp::Sub => Some(ConstValue::Integer(
                        l.as_integer()?.wrapping_sub(r.as_integer()?),
                    )),
                    BinOp::Mul => Some(ConstValue::Integer(
                        l.as_integer()?.wrapping_mul(r.as_integer()?),
                    )),
                    BinOp::Div => {
                        let r = r.as_integer()?;
                        if r == 0 {
                            None
                        } else {
                            l.as_integer()?.checked_div(r).map(ConstValue::Integer)
                        }
                    }
                    BinOp::Mod => {
                        let r = r.as_integer()?;
                        if r == 0 {
                            None
                        } else {
                            l.as_integer()?.checked_rem(r).map(ConstValue::Integer)
                        }
                    }
                    BinOp::Eq => match (l, r) {
                        (ConstValue::Integer(a), ConstValue::Integer(b)) => {
                            Some(ConstValue::Bool(a == b))
                        }
                        (ConstValue::Bool(a), ConstValue::Bool(b)) => {
                            Some(ConstValue::Bool(a == b))
                        }
                        _ => None,
                    },
                    BinOp::Ne => match (l, r) {
                        (ConstValue::Integer(a), ConstValue::Integer(b)) => {
                            Some(ConstValue::Bool(a != b))
                        }
                        (ConstValue::Bool(a), ConstValue::Bool(b)) => {
                            Some(ConstValue::Bool(a != b))
                        }
                        _ => None,
                    },
                    BinOp::Lt => Some(ConstValue::Bool(l.as_integer()? < r.as_integer()?)),
                    BinOp::Gt => Some(ConstValue::Bool(l.as_integer()? > r.as_integer()?)),
                    BinOp::Le => Some(ConstValue::Bool(l.as_integer()? <= r.as_integer()?)),
                    BinOp::Ge => Some(ConstValue::Bool(l.as_integer()? >= r.as_integer()?)),
                    BinOp::And => Some(ConstValue::Bool(l.as_bool()? && r.as_bool()?)),
                    BinOp::Or => Some(ConstValue::Bool(l.as_bool()? || r.as_bool()?)),
                    BinOp::BitAnd => Some(ConstValue::Integer(l.as_integer()? & r.as_integer()?)),
                    BinOp::BitOr => Some(ConstValue::Integer(l.as_integer()? | r.as_integer()?)),
                    BinOp::BitXor => Some(ConstValue::Integer(l.as_integer()? ^ r.as_integer()?)),
                    // Only constant-fold small shift amounts to avoid type-width issues.
                    // For larger shifts, defer to runtime where hardware handles masking.
                    BinOp::Shl => {
                        let r = r.as_integer()?;
                        if !(0..8).contains(&r) {
                            return None;
                        }
                        Some(ConstValue::Integer(l.as_integer()? << r))
                    }
                    BinOp::Shr => {
                        let r = r.as_integer()?;
                        if !(0..8).contains(&r) {
                            return None;
                        }
                        Some(ConstValue::Integer(l.as_integer()? >> r))
                    }
                }
            }

            // Comptime block: comptime { expr } is compile-time evaluable if its inner expr is
            InstData::Comptime { expr } => self.try_evaluate_const(*expr),

            // Block: evaluate the result expression (last expression in the block)
            InstData::Block { extra_start, len } => {
                // A block is comptime-evaluable if it has a single instruction
                // (which is the result expression) OR if all statements are
                // side-effect-free and the result is comptime-evaluable.
                // For now, only handle the single-instruction case (common for
                // simple type-returning functions like `fn make_type() -> type { i32 }`).
                if *len == 1 {
                    let inst_refs = self.rir.get_extra(*extra_start, *len);
                    let result_ref = InstRef::from_raw(inst_refs[0]);
                    self.try_evaluate_const(result_ref)
                } else {
                    None // Blocks with multiple instructions need full interpreter support
                }
            }

            // Anonymous struct type: evaluate to a comptime type value
            InstData::AnonStructType {
                fields_start,
                fields_len,
                methods_start,
                methods_len,
                directives_start,
                directives_len,
            } => {
                // Get the field declarations from the RIR
                let field_decls = self.rir.get_field_decls(*fields_start, *fields_len);

                // Resolve each field type and build the struct fields
                let mut struct_fields = Vec::with_capacity(field_decls.len());
                for (name_sym, type_sym) in field_decls {
                    let name_str = self.interner.resolve(&name_sym).to_string();
                    // Try to resolve the type - for anonymous structs in comptime context,
                    // we need to be able to resolve the field types
                    let field_ty = self.resolve_type_for_comptime(type_sym)?;
                    struct_fields.push(StructField {
                        name: name_str,
                        ty: field_ty,

                        is_pub: true,
                    });
                }

                // Extract method signatures for structural equality comparison
                let method_sigs = self.extract_anon_method_sigs(*methods_start, *methods_len);

                // Find or create the anonymous struct type
                let (struct_ty, is_new) = self.find_or_create_anon_struct(
                    &struct_fields,
                    &method_sigs,
                    &HashMap::default(),
                );

                // Register methods if present and struct is new
                // This handles non-comptime functions like `fn Counter() -> type { struct { fn get() {} } }`
                // For comptime functions with captured values, use try_evaluate_const_with_subst instead
                if is_new && *methods_len > 0 {
                    let struct_id = struct_ty.as_struct()?;
                    // Use comptime-safe method registration (no type subst, no value subst for non-comptime)
                    self.register_anon_struct_methods_for_comptime_with_subst(
                        AnonStructSpec {
                            struct_id,
                            struct_type: struct_ty,
                            methods_start: *methods_start,
                            methods_len: *methods_len,
                        },
                        inst.span,
                        &HashMap::default(), // Empty type substitution
                        &HashMap::default(), // Empty value substitution (non-comptime)
                    )?;
                }
                // ADR-0058: splice `@derive(...)` directives on the anon
                // struct expression. Runs only on the new-StructId path so
                // identical parameterizations don't double-splice. Errors
                // are converted to None so the comptime evaluator gives
                // up on this value (matching the rest of the path).
                if is_new
                    && *directives_len > 0
                    && let Some(struct_id) = struct_ty.as_struct()
                    && let Err(e) = self.splice_anon_struct_derives(
                        struct_id,
                        *directives_start,
                        *directives_len,
                    )
                {
                    self.pending_anon_derive_errors.push(e);
                    return None;
                }
                Some(ConstValue::Type(struct_ty))
            }

            // Anonymous interface type (ADR-0057): evaluate to a comptime
            // type value carrying the freshly-built (or deduped) InterfaceId.
            InstData::AnonInterfaceType {
                methods_start,
                methods_len,
            } => {
                let methods = self
                    .build_anon_interface_def(
                        *methods_start,
                        *methods_len,
                        inst.span,
                        &HashMap::default(),
                    )
                    .ok()?;
                let iface_id = self.find_or_create_anon_interface(methods);
                Some(ConstValue::Type(Type::new_interface(iface_id)))
            }

            // Anonymous enum type: evaluate to a comptime type value
            InstData::AnonEnumType {
                variants_start,
                variants_len,
                methods_start,
                methods_len,
                directives_start,
                directives_len,
            } => {
                let variant_decls = self
                    .rir
                    .get_enum_variant_decls(*variants_start, *variants_len);

                let mut enum_variants = Vec::with_capacity(variant_decls.len());
                for (name_sym, field_type_syms, field_name_syms) in &variant_decls {
                    let name_str = self.interner.resolve(name_sym).to_string();
                    let mut fields = Vec::with_capacity(field_type_syms.len());
                    for ty_sym in field_type_syms {
                        let field_ty = self.resolve_type_for_comptime(*ty_sym)?;
                        fields.push(field_ty);
                    }
                    let field_names: Vec<String> = field_name_syms
                        .iter()
                        .map(|s| self.interner.resolve(s).to_string())
                        .collect();
                    enum_variants.push(EnumVariantDef {
                        name: name_str,
                        fields,
                        field_names,
                    });
                }

                let method_sigs = self.extract_anon_method_sigs(*methods_start, *methods_len);

                let (enum_ty, is_new) = self.find_or_create_anon_enum(
                    &enum_variants,
                    &method_sigs,
                    &HashMap::default(),
                );

                // Register methods for newly created anonymous enums
                if is_new
                    && *methods_len > 0
                    && let TypeKind::Enum(enum_id) = enum_ty.kind()
                {
                    self.register_anon_enum_methods_for_comptime_with_subst(
                        enum_id,
                        enum_ty,
                        *methods_start,
                        *methods_len,
                        &HashMap::default(),
                    );
                }
                // ADR-0058: splice `@derive(...)` on the anon enum
                // expression for the non-substitution comptime path.
                if is_new
                    && *directives_len > 0
                    && let TypeKind::Enum(enum_id) = enum_ty.kind()
                    && let Err(e) =
                        self.splice_anon_enum_derives(enum_id, *directives_start, *directives_len)
                {
                    self.pending_anon_derive_errors.push(e);
                    return None;
                }

                Some(ConstValue::Type(enum_ty))
            }

            // TypeConst: a type used as a value (e.g., `i32` in `identity(i32, 42)`)
            InstData::TypeConst { type_name } => {
                let type_name_str = self.interner.resolve(type_name);
                let ty = match type_name_str {
                    "i8" => Type::I8,
                    "i16" => Type::I16,
                    "i32" => Type::I32,
                    "i64" => Type::I64,
                    "u8" => Type::U8,
                    "u16" => Type::U16,
                    "u32" => Type::U32,
                    "u64" => Type::U64,
                    "isize" => Type::ISIZE,
                    "usize" => Type::USIZE,
                    "f16" => Type::F16,
                    "f32" => Type::F32,
                    "f64" => Type::F64,
                    "bool" => Type::BOOL,
                    "char" => Type::CHAR,
                    "()" => Type::UNIT,
                    "!" => Type::NEVER,
                    _ => {
                        // Check for struct types
                        if let Some(&struct_id) = self.structs.get(type_name) {
                            Type::new_struct(struct_id)
                        } else if let Some(&enum_id) = self.enums.get(type_name) {
                            Type::new_enum(enum_id)
                        } else {
                            return None; // Unknown type
                        }
                    }
                };
                Some(ConstValue::Type(ty))
            }

            // VarRef: when a variable reference is actually a type name (e.g., `Point` in `fn make_type() -> type { Point }`)
            InstData::VarRef { name } => {
                // Try to resolve as a type - if it's a type name, return the type
                let name_str = self.interner.resolve(name);
                let ty = match name_str {
                    "i8" => Type::I8,
                    "i16" => Type::I16,
                    "i32" => Type::I32,
                    "i64" => Type::I64,
                    "u8" => Type::U8,
                    "u16" => Type::U16,
                    "u32" => Type::U32,
                    "u64" => Type::U64,
                    "isize" => Type::ISIZE,
                    "usize" => Type::USIZE,
                    "f16" => Type::F16,
                    "f32" => Type::F32,
                    "f64" => Type::F64,
                    "bool" => Type::BOOL,
                    "char" => Type::CHAR,
                    "()" => Type::UNIT,
                    "!" => Type::NEVER,
                    _ => {
                        // Check for struct types
                        if let Some(&struct_id) = self.structs.get(name) {
                            Type::new_struct(struct_id)
                        } else if let Some(&enum_id) = self.enums.get(name) {
                            Type::new_enum(enum_id)
                        } else {
                            return None; // Not a type name - can't evaluate at compile time
                        }
                    }
                };
                Some(ConstValue::Type(ty))
            }

            // Everything else requires runtime evaluation
            _ => None,
        }
    }

    /// Try to extract a constant integer value from an RIR index expression.
    ///
    /// This is used for compile-time bounds checking. Returns `Some(value)` if
    /// the index can be evaluated to an integer constant at compile time.
    pub(crate) fn try_get_const_index(&mut self, inst_ref: InstRef) -> Option<i64> {
        self.try_evaluate_const(inst_ref)?.as_integer()
    }

    /// Try to evaluate an RIR instruction to a compile-time constant value with type substitution.
    ///
    /// This is used when evaluating generic functions that return `type`. For example,
    /// when calling `fn Pair(comptime T: type) -> type { struct { first: T, second: T } }`
    /// with `Pair(i32)`, we need to substitute `T -> i32` when evaluating the body.
    ///
    /// The `type_subst` map contains mappings from type parameter names to concrete types.
    pub(crate) fn try_evaluate_const_with_subst(
        &mut self,
        inst_ref: InstRef,
        type_subst: &rustc_hash::FxHashMap<Spur, Type>,
        value_subst: &rustc_hash::FxHashMap<Spur, ConstValue>,
    ) -> Option<ConstValue> {
        let inst = self.rir.get(inst_ref);
        match &inst.data {
            // Integer literals
            InstData::IntConst(value) => i64::try_from(*value).ok().map(ConstValue::Integer),

            // Boolean literals
            InstData::BoolConst(value) => Some(ConstValue::Bool(*value)),

            InstData::Unary { op, operand } => {
                let v = self.try_evaluate_const_with_subst(*operand, type_subst, value_subst)?;
                match (op, v) {
                    (UnaryOp::Neg, ConstValue::Integer(n)) => {
                        Some(ConstValue::Integer(n.wrapping_neg()))
                    }
                    (UnaryOp::Not, ConstValue::Bool(b)) => Some(ConstValue::Bool(!b)),
                    (UnaryOp::BitNot, ConstValue::Integer(n)) => Some(ConstValue::Integer(!n)),
                    _ => None,
                }
            }

            InstData::Bin { op, lhs, rhs } => {
                let l = self.try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?;
                let r = self.try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?;
                match op {
                    BinOp::Add => Some(ConstValue::Integer(
                        l.as_integer()?.wrapping_add(r.as_integer()?),
                    )),
                    BinOp::Sub => Some(ConstValue::Integer(
                        l.as_integer()?.wrapping_sub(r.as_integer()?),
                    )),
                    BinOp::Mul => Some(ConstValue::Integer(
                        l.as_integer()?.wrapping_mul(r.as_integer()?),
                    )),
                    BinOp::Div => {
                        let r = r.as_integer()?;
                        if r == 0 {
                            None
                        } else {
                            l.as_integer()?.checked_div(r).map(ConstValue::Integer)
                        }
                    }
                    BinOp::Mod => {
                        let r = r.as_integer()?;
                        if r == 0 {
                            None
                        } else {
                            l.as_integer()?.checked_rem(r).map(ConstValue::Integer)
                        }
                    }
                    BinOp::Eq => match (l, r) {
                        (ConstValue::Integer(a), ConstValue::Integer(b)) => {
                            Some(ConstValue::Bool(a == b))
                        }
                        (ConstValue::Bool(a), ConstValue::Bool(b)) => {
                            Some(ConstValue::Bool(a == b))
                        }
                        _ => None,
                    },
                    BinOp::Ne => match (l, r) {
                        (ConstValue::Integer(a), ConstValue::Integer(b)) => {
                            Some(ConstValue::Bool(a != b))
                        }
                        (ConstValue::Bool(a), ConstValue::Bool(b)) => {
                            Some(ConstValue::Bool(a != b))
                        }
                        _ => None,
                    },
                    BinOp::Lt => Some(ConstValue::Bool(l.as_integer()? < r.as_integer()?)),
                    BinOp::Gt => Some(ConstValue::Bool(l.as_integer()? > r.as_integer()?)),
                    BinOp::Le => Some(ConstValue::Bool(l.as_integer()? <= r.as_integer()?)),
                    BinOp::Ge => Some(ConstValue::Bool(l.as_integer()? >= r.as_integer()?)),
                    BinOp::And => Some(ConstValue::Bool(l.as_bool()? && r.as_bool()?)),
                    BinOp::Or => Some(ConstValue::Bool(l.as_bool()? || r.as_bool()?)),
                    BinOp::BitAnd => Some(ConstValue::Integer(l.as_integer()? & r.as_integer()?)),
                    BinOp::BitOr => Some(ConstValue::Integer(l.as_integer()? | r.as_integer()?)),
                    BinOp::BitXor => Some(ConstValue::Integer(l.as_integer()? ^ r.as_integer()?)),
                    BinOp::Shl => {
                        let r = r.as_integer()?;
                        if !(0..8).contains(&r) {
                            return None;
                        }
                        Some(ConstValue::Integer(l.as_integer()? << r))
                    }
                    BinOp::Shr => {
                        let r = r.as_integer()?;
                        if !(0..8).contains(&r) {
                            return None;
                        }
                        Some(ConstValue::Integer(l.as_integer()? >> r))
                    }
                }
            }

            // Comptime block: comptime { expr } is compile-time evaluable if its inner expr is
            InstData::Comptime { expr } => {
                self.try_evaluate_const_with_subst(*expr, type_subst, value_subst)
            }

            // Block: evaluate the result expression (last expression in the block)
            InstData::Block { extra_start, len } => {
                if *len == 1 {
                    let inst_refs = self.rir.get_extra(*extra_start, *len);
                    let result_ref = InstRef::from_raw(inst_refs[0]);
                    self.try_evaluate_const_with_subst(result_ref, type_subst, value_subst)
                } else {
                    None
                }
            }

            // Anonymous struct type: evaluate to a comptime type value with substitution
            InstData::AnonStructType {
                fields_start,
                fields_len,
                methods_start,
                methods_len,
                directives_start,
                directives_len,
            } => {
                let field_decls = self.rir.get_field_decls(*fields_start, *fields_len);

                // Empty structs (no fields, no methods) are not allowed —
                // mirrors the `EmptyStruct` check in the runtime analysis path
                // (`analyze_inst` for `AnonStructType`). We need this here
                // because type-constructor function bodies are now evaluated
                // exclusively via this comptime path.
                if field_decls.is_empty() && *methods_len == 0 {
                    self.pending_anon_eval_errors
                        .push(CompileError::new(ErrorKind::EmptyStruct, inst.span));
                    return None;
                }

                let mut struct_fields = Vec::with_capacity(field_decls.len());
                for (name_sym, type_sym) in field_decls {
                    let name_str = self.interner.resolve(&name_sym).to_string();
                    // Use the substitution-aware type resolution
                    let field_ty =
                        self.resolve_type_for_comptime_with_subst(type_sym, type_subst)?;
                    struct_fields.push(StructField {
                        name: name_str,
                        ty: field_ty,

                        is_pub: true,
                    });
                }

                // Detect duplicate method names up front so behaviour is the
                // same whether or not a previous evaluation attempt partially
                // populated `self.methods`. (`register_anon_struct_methods_*`
                // also catches this, but only after registering the first
                // copy — which would make a retry after a fall-through
                // silently succeed.)
                if *methods_len > 0 {
                    let method_refs = self.rir.get_inst_refs(*methods_start, *methods_len);
                    let mut seen: HashSet<Spur> = HashSet::default();
                    for mref in method_refs {
                        if let InstData::FnDecl {
                            name: method_name, ..
                        } = &self.rir.get(mref).data
                            && !seen.insert(*method_name)
                        {
                            let method_name_str = self.interner.resolve(method_name).to_string();
                            self.pending_anon_eval_errors.push(CompileError::new(
                                ErrorKind::DuplicateMethod {
                                    type_name: "anonymous struct".to_string(),
                                    method_name: method_name_str,
                                },
                                inst.span,
                            ));
                            return None;
                        }
                    }
                }

                // Extract method signatures for structural equality comparison
                let method_sigs = self.extract_anon_method_sigs(*methods_start, *methods_len);

                let (struct_ty, is_new) =
                    self.find_or_create_anon_struct(&struct_fields, &method_sigs, value_subst);

                // Register methods if present (requires preview feature)
                // Register if either:
                // 1. This is a newly created struct (is_new=true), OR
                // 2. The struct exists but has no methods registered yet
                if *methods_len > 0 {
                    let struct_id = struct_ty.as_struct()?;

                    // Check if methods are already registered for this struct
                    let method_refs = self.rir.get_inst_refs(*methods_start, *methods_len);
                    let first_method_ref = method_refs[0];
                    let first_method_inst = self.rir.get(first_method_ref);
                    if let InstData::FnDecl {
                        name: method_name, ..
                    } = &first_method_inst.data
                    {
                        let needs_registration =
                            !self.methods.contains_key(&(struct_id, *method_name));

                        if needs_registration {
                            // Use comptime-safe method registration with type substitution
                            self.register_anon_struct_methods_for_comptime_with_subst(
                                AnonStructSpec {
                                    struct_id,
                                    struct_type: struct_ty,
                                    methods_start: *methods_start,
                                    methods_len: *methods_len,
                                },
                                inst.span,
                                type_subst,
                                value_subst,
                            )?;
                        }
                    }
                }
                // ADR-0058: splice `@derive(...)` on the anon struct
                // expression for parameterized comptime calls. Each fresh
                // `StructId` (per parameterization) gets its own splice.
                if is_new
                    && *directives_len > 0
                    && let Some(struct_id) = struct_ty.as_struct()
                    && let Err(e) = self.splice_anon_struct_derives(
                        struct_id,
                        *directives_start,
                        *directives_len,
                    )
                {
                    self.pending_anon_derive_errors.push(e);
                    return None;
                }
                // ADR-0082: if we're inside the lang-item Vec function's
                // evaluation, register this StructId as a Vec instance for
                // the captured element type.
                if let Some(ctor_fn) = self.comptime_ctor_fn
                    && Some(ctor_fn) == self.lang_items.vec_fn()
                    && let Some(struct_id) = struct_ty.as_struct()
                {
                    let info = self.functions.get(&ctor_fn);
                    if let Some(info) = info {
                        let param_names = self.param_arena.names(info.params);
                        if let Some(elem_ty) =
                            param_names.first().and_then(|n| type_subst.get(n).copied())
                        {
                            self.vec_instance_registry.insert(struct_id, elem_ty);
                        }
                    }
                }
                Some(ConstValue::Type(struct_ty))
            }

            // Anonymous interface type with substitution (ADR-0057):
            // resolve method-sig types under `type_subst` (and the regular
            // comptime-resolver path for `T` references) before deduping.
            // `value_subst` is unused — interfaces have no captured values.
            InstData::AnonInterfaceType {
                methods_start,
                methods_len,
            } => {
                let methods = self
                    .build_anon_interface_def(*methods_start, *methods_len, inst.span, type_subst)
                    .ok()?;
                let _ = value_subst;
                let iface_id = self.find_or_create_anon_interface(methods);
                Some(ConstValue::Type(Type::new_interface(iface_id)))
            }

            // Anonymous enum type: evaluate to a comptime type value with substitution
            InstData::AnonEnumType {
                variants_start,
                variants_len,
                methods_start,
                methods_len,
                directives_start,
                directives_len,
            } => {
                let variant_decls = self
                    .rir
                    .get_enum_variant_decls(*variants_start, *variants_len);

                // Empty enums are not allowed — mirrors the runtime
                // analysis path's `EmptyAnonEnum` check.
                if variant_decls.is_empty() {
                    self.pending_anon_eval_errors
                        .push(CompileError::new(ErrorKind::EmptyAnonEnum, inst.span));
                    return None;
                }

                // Detect duplicate method names up front (parallel to the
                // anon-struct case). Without this, type-constructor enum
                // bodies would silently accept duplicate method declarations
                // because runtime body analysis is now skipped for them.
                if *methods_len > 0 {
                    let method_refs = self.rir.get_inst_refs(*methods_start, *methods_len);
                    let mut seen: HashSet<Spur> = HashSet::default();
                    for mref in method_refs {
                        if let InstData::FnDecl {
                            name: method_name, ..
                        } = &self.rir.get(mref).data
                            && !seen.insert(*method_name)
                        {
                            let method_name_str = self.interner.resolve(method_name).to_string();
                            self.pending_anon_eval_errors.push(CompileError::new(
                                ErrorKind::DuplicateMethod {
                                    type_name: "anonymous enum".to_string(),
                                    method_name: method_name_str,
                                },
                                inst.span,
                            ));
                            return None;
                        }
                    }
                }

                let mut enum_variants = Vec::with_capacity(variant_decls.len());
                for (name_sym, field_type_syms, field_name_syms) in &variant_decls {
                    let name_str = self.interner.resolve(name_sym).to_string();
                    let mut fields = Vec::with_capacity(field_type_syms.len());
                    for ty_sym in field_type_syms {
                        let field_ty =
                            self.resolve_type_for_comptime_with_subst(*ty_sym, type_subst)?;
                        fields.push(field_ty);
                    }
                    let field_names: Vec<String> = field_name_syms
                        .iter()
                        .map(|s| self.interner.resolve(s).to_string())
                        .collect();
                    enum_variants.push(EnumVariantDef {
                        name: name_str,
                        fields,
                        field_names,
                    });
                }

                let method_sigs = self.extract_anon_method_sigs(*methods_start, *methods_len);

                let (enum_ty, is_new) =
                    self.find_or_create_anon_enum(&enum_variants, &method_sigs, value_subst);

                // Register methods for newly created anonymous enums with captured values
                if is_new
                    && *methods_len > 0
                    && let TypeKind::Enum(enum_id) = enum_ty.kind()
                {
                    self.register_anon_enum_methods_for_comptime_with_subst(
                        enum_id,
                        enum_ty,
                        *methods_start,
                        *methods_len,
                        type_subst,
                    );
                }
                // ADR-0058: splice `@derive(...)` on the anon enum
                // expression with substitution. Each fresh `EnumId` gets
                // its own splice.
                if is_new
                    && *directives_len > 0
                    && let TypeKind::Enum(enum_id) = enum_ty.kind()
                    && let Err(e) =
                        self.splice_anon_enum_derives(enum_id, *directives_start, *directives_len)
                {
                    self.pending_anon_derive_errors.push(e);
                    return None;
                }

                Some(ConstValue::Type(enum_ty))
            }

            // TypeConst: a type used as a value
            InstData::TypeConst { type_name } => {
                // First check the substitution map
                if let Some(&ty) = type_subst.get(type_name) {
                    return Some(ConstValue::Type(ty));
                }

                let type_name_str = self.interner.resolve(type_name);
                let ty = match type_name_str {
                    "i8" => Type::I8,
                    "i16" => Type::I16,
                    "i32" => Type::I32,
                    "i64" => Type::I64,
                    "u8" => Type::U8,
                    "u16" => Type::U16,
                    "u32" => Type::U32,
                    "u64" => Type::U64,
                    "isize" => Type::ISIZE,
                    "usize" => Type::USIZE,
                    "f16" => Type::F16,
                    "f32" => Type::F32,
                    "f64" => Type::F64,
                    "bool" => Type::BOOL,
                    "char" => Type::CHAR,
                    "()" => Type::UNIT,
                    "!" => Type::NEVER,
                    _ => {
                        if let Some(&struct_id) = self.structs.get(type_name) {
                            Type::new_struct(struct_id)
                        } else if let Some(&enum_id) = self.enums.get(type_name) {
                            Type::new_enum(enum_id)
                        } else {
                            return None;
                        }
                    }
                };
                Some(ConstValue::Type(ty))
            }

            // VarRef: check substitution maps first, then try as a type name
            InstData::VarRef { name } => {
                // Check if this is a type parameter in the type substitution map
                if let Some(&ty) = type_subst.get(name) {
                    return Some(ConstValue::Type(ty));
                }

                // Check if this is a comptime value variable in the value substitution map
                if let Some(value) = value_subst.get(name) {
                    return Some(*value);
                }

                // Try to resolve as a type name
                let name_str = self.interner.resolve(name);
                let ty = match name_str {
                    "i8" => Type::I8,
                    "i16" => Type::I16,
                    "i32" => Type::I32,
                    "i64" => Type::I64,
                    "u8" => Type::U8,
                    "u16" => Type::U16,
                    "u32" => Type::U32,
                    "u64" => Type::U64,
                    "isize" => Type::ISIZE,
                    "usize" => Type::USIZE,
                    "f16" => Type::F16,
                    "f32" => Type::F32,
                    "f64" => Type::F64,
                    "bool" => Type::BOOL,
                    "char" => Type::CHAR,
                    "()" => Type::UNIT,
                    "!" => Type::NEVER,
                    _ => {
                        if let Some(&struct_id) = self.structs.get(name) {
                            Type::new_struct(struct_id)
                        } else if let Some(&enum_id) = self.enums.get(name) {
                            Type::new_enum(enum_id)
                        } else {
                            return None;
                        }
                    }
                };
                Some(ConstValue::Type(ty))
            }

            // Everything else requires runtime evaluation
            _ => None,
        }
    }

    // =========================================================================
    // Phase 1a: Stateful Comptime Interpreter
    // =========================================================================
    //
    // The interpreter extends `try_evaluate_const` with mutable local variable
    // state, enabling:
    //   - Multi-statement `comptime { ... }` blocks
    //   - `let` bindings within comptime blocks
    //   - Variable assignment within comptime blocks
    //   - `if`/`else` with comptime-evaluable conditions
    //   - `while`, `loop`, `break`, `continue` (Phase 1b)
    //   - Function calls, push/pop call frames (Phase 1c)
    //   - `ConstValue::Struct`, `ConstValue::Array` (Phase 1d)
    //   - Comptime arg evaluation via full interpreter (Phase 1e)

    /// Try to evaluate a single expression as a comptime argument value.
    ///
    /// Used when validating and extracting values for `comptime` parameters at
    /// call sites (Phase 1e). First tries the lightweight non-stateful evaluator
    /// (fast for literals and arithmetic), then falls back to the full stateful
    /// interpreter which supports function calls and composite operations.
    ///
    /// Returns `Some(value)` if evaluable at compile time, `None` otherwise.
    /// Never returns control-flow signals (`BreakSignal`, `ContinueSignal`,
    /// `ReturnSignal`).
    pub(crate) fn try_evaluate_comptime_arg(
        &mut self,
        inst_ref: InstRef,
        ctx: &AnalysisContext,
        outer_span: Span,
    ) -> Option<ConstValue> {
        // Fast path: lightweight evaluator handles literals and arithmetic.
        if let Some(val) = self.try_evaluate_const(inst_ref) {
            return Some(val);
        }
        // Full stateful interpreter: supports function calls, let bindings, etc.
        // Save and restore step counter so arg evaluation doesn't consume the
        // budget of any outer comptime block that may be in progress.
        let prev_steps = self.comptime_steps_used;
        self.comptime_steps_used = 0;
        let mut locals = ctx.comptime_value_vars.clone();
        let result = self
            .evaluate_comptime_inst(inst_ref, &mut locals, ctx, outer_span)
            .ok();
        self.comptime_steps_used = prev_steps;
        // Filter out control-flow signals — they cannot be meaningful here.
        result.filter(|v| {
            !matches!(
                v,
                ConstValue::BreakSignal | ConstValue::ContinueSignal | ConstValue::ReturnSignal
            )
        })
    }

    /// Format a comptime value as a human-readable string.
    ///
    /// Used by `@dbg` and `@compileLog` to render comptime values during
    /// compile-time evaluation.
    fn format_const_value(&self, val: ConstValue, span: Span) -> CompileResult<String> {
        match val {
            ConstValue::Bool(b) => Ok(if b {
                "true".to_string()
            } else {
                "false".to_string()
            }),
            ConstValue::Integer(v) => Ok(format!("{v}")),
            ConstValue::Unit => Ok("()".to_string()),
            ConstValue::ComptimeStr(idx) => match &self.comptime_heap[idx as usize] {
                ComptimeHeapItem::String(s) => Ok(s.clone()),
                _ => Err(CompileError::new(
                    ErrorKind::ComptimeEvaluationFailed {
                        reason: "invalid comptime_str heap reference".into(),
                    },
                    span,
                )),
            },
            _ => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "expression contains values that cannot be known at compile time"
                        .into(),
                },
                span,
            )),
        }
    }

    /// Resolve a `ConstValue::ComptimeStr` to its Rust string content.
    pub(crate) fn resolve_comptime_str(&self, idx: u32, span: Span) -> CompileResult<&str> {
        match &self.comptime_heap[idx as usize] {
            ComptimeHeapItem::String(s) => Ok(s.as_str()),
            _ => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "invalid comptime_str heap reference".into(),
                },
                span,
            )),
        }
    }

    /// Evaluate a `comptime_str` method call in the comptime interpreter.
    ///
    /// Dispatches methods like `len`, `is_empty`, `contains`, `starts_with`,
    /// `ends_with`, `eq`, `ne`, `lt`, `le`, `gt`, `ge`, and `concat`.
    fn evaluate_comptime_str_method(
        &mut self,
        str_idx: u32,
        method_name: &str,
        call_args: &[gruel_rir::RirCallArg],
        locals: &mut HashMap<Spur, ConstValue>,
        ctx: &AnalysisContext,
        spans: ComptimeSpans,
    ) -> CompileResult<ConstValue> {
        let ComptimeSpans {
            outer: outer_span,
            inst: inst_span,
        } = spans;
        let s = self.resolve_comptime_str(str_idx, inst_span)?.to_string();

        match method_name {
            "len" => {
                if !call_args.is_empty() {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicWrongArgCount {
                            name: "len".to_string(),
                            expected: 0,
                            found: call_args.len(),
                        },
                        inst_span,
                    ));
                }
                Ok(ConstValue::Integer(s.len() as i64))
            }
            "is_empty" => {
                if !call_args.is_empty() {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicWrongArgCount {
                            name: "is_empty".to_string(),
                            expected: 0,
                            found: call_args.len(),
                        },
                        inst_span,
                    ));
                }
                Ok(ConstValue::Bool(s.is_empty()))
            }
            "contains" | "starts_with" | "ends_with" | "eq" | "ne" | "lt" | "le" | "gt" | "ge" => {
                if call_args.len() != 1 {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicWrongArgCount {
                            name: method_name.to_string(),
                            expected: 1,
                            found: call_args.len(),
                        },
                        inst_span,
                    ));
                }
                let arg_val =
                    self.evaluate_comptime_inst(call_args[0].value, locals, ctx, outer_span)?;
                let other_idx = match arg_val {
                    ConstValue::ComptimeStr(idx) => idx,
                    _ => {
                        return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: format!(
                                    "comptime_str.{method_name} expects a comptime_str argument"
                                ),
                            },
                            inst_span,
                        ));
                    }
                };
                let other = self.resolve_comptime_str(other_idx, inst_span)?.to_string();
                let result = match method_name {
                    "contains" => s.contains(other.as_str()),
                    "starts_with" => s.starts_with(other.as_str()),
                    "ends_with" => s.ends_with(other.as_str()),
                    "eq" => s == other,
                    "ne" => s != other,
                    "lt" => s < other,
                    "le" => s <= other,
                    "gt" => s > other,
                    "ge" => s >= other,
                    _ => unreachable!(),
                };
                Ok(ConstValue::Bool(result))
            }
            "concat" => {
                if call_args.len() != 1 {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicWrongArgCount {
                            name: "concat".to_string(),
                            expected: 1,
                            found: call_args.len(),
                        },
                        inst_span,
                    ));
                }
                let arg_val =
                    self.evaluate_comptime_inst(call_args[0].value, locals, ctx, outer_span)?;
                let other_idx = match arg_val {
                    ConstValue::ComptimeStr(idx) => idx,
                    _ => {
                        return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: "comptime_str.concat expects a comptime_str argument"
                                    .into(),
                            },
                            inst_span,
                        ));
                    }
                };
                let other = self.resolve_comptime_str(other_idx, inst_span)?.to_string();
                let result = format!("{s}{other}");
                let idx = self.comptime_heap.len() as u32;
                self.comptime_heap.push(ComptimeHeapItem::String(result));
                Ok(ConstValue::ComptimeStr(idx))
            }
            "clone" => {
                if !call_args.is_empty() {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicWrongArgCount {
                            name: "clone".to_string(),
                            expected: 0,
                            found: call_args.len(),
                        },
                        inst_span,
                    ));
                }
                Ok(self.alloc_comptime_str(s))
            }
            "push_str" | "push" | "clear" | "reserve" => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "cannot call .{method_name}() on a compile-time string; use .concat() to produce a new string"
                    ),
                },
                inst_span,
            )),
            "capacity" => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "capacity is not available for compile-time strings".into(),
                },
                inst_span,
            )),
            _ => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!("unknown comptime_str method '{method_name}'"),
                },
                inst_span,
            )),
        }
    }

    /// Evaluate a comptime intrinsic argument as a string.
    ///
    /// Accepts both `StringConst` instructions (string literals) and
    /// `ConstValue::ComptimeStr` values from comptime evaluation.
    fn evaluate_comptime_string_arg(
        &mut self,
        arg_ref: InstRef,
        locals: &mut HashMap<Spur, ConstValue>,
        ctx: &AnalysisContext,
        outer_span: Span,
    ) -> CompileResult<String> {
        let arg_inst = self.rir.get(arg_ref);
        // Try string literal first
        if let gruel_rir::InstData::StringConst(spur) = &arg_inst.data {
            return Ok(self.interner.resolve(spur).to_string());
        }
        // Otherwise evaluate as a comptime expression
        let val = self.evaluate_comptime_inst(arg_ref, locals, ctx, outer_span)?;
        match val {
            ConstValue::ComptimeStr(idx) => self
                .resolve_comptime_str(idx, arg_inst.span)
                .map(|s| s.to_string()),
            _ => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "@compile_error requires a string literal or comptime_str argument"
                        .into(),
                },
                arg_inst.span,
            )),
        }
    }

    /// Allocate a `comptime_str` on the comptime heap and return a `ConstValue::ComptimeStr`.
    fn alloc_comptime_str(&mut self, s: String) -> ConstValue {
        let idx = self.comptime_heap.len() as u32;
        self.comptime_heap.push(ComptimeHeapItem::String(s));
        ConstValue::ComptimeStr(idx)
    }

    /// Allocate a comptime struct on the heap and return a `ConstValue::Struct`.
    fn alloc_comptime_struct(
        &mut self,
        struct_id: StructId,
        fields: Vec<ConstValue>,
    ) -> ConstValue {
        let idx = self.comptime_heap.len() as u32;
        self.comptime_heap
            .push(ComptimeHeapItem::Struct { struct_id, fields });
        ConstValue::Struct(idx)
    }

    /// Allocate a comptime array on the heap and return a `ConstValue::Array`.
    fn alloc_comptime_array(&mut self, elements: Vec<ConstValue>) -> ConstValue {
        let idx = self.comptime_heap.len() as u32;
        self.comptime_heap.push(ComptimeHeapItem::Array(elements));
        ConstValue::Array(idx)
    }

    /// Resolve a `TypeKind` variant name to its discriminant index.
    fn typekind_variant_idx(&self, variant_name: &str) -> u32 {
        let enum_id = self
            .builtin_typekind_id
            .expect("TypeKind enum not injected");
        let enum_def = self.type_pool.enum_def(enum_id);
        enum_def
            .find_variant(variant_name)
            .unwrap_or_else(|| panic!("TypeKind variant '{variant_name}' not found")) as u32
    }

    /// Evaluate `@type_name(T)` — returns the type's name as a `comptime_str`.
    fn evaluate_comptime_type_name(&mut self, ty: Type, _span: Span) -> CompileResult<ConstValue> {
        let name = self.type_pool.format_type_name(ty);
        Ok(self.alloc_comptime_str(name))
    }

    /// Evaluate `@type_info(T)` — returns a comptime struct describing the type.
    fn evaluate_comptime_type_info(&mut self, ty: Type, span: Span) -> CompileResult<ConstValue> {
        let typekind_enum_id = self
            .builtin_typekind_id
            .expect("TypeKind enum not injected");
        let typekind_type = Type::new_enum(typekind_enum_id);

        match ty.kind() {
            TypeKind::Struct(struct_id) => {
                self.build_struct_type_info(struct_id, typekind_enum_id, typekind_type)
            }
            TypeKind::Enum(enum_id) => {
                self.build_enum_type_info(enum_id, typekind_enum_id, typekind_type)
            }
            TypeKind::I8 => {
                self.build_int_type_info("i8", 8, true, typekind_enum_id, typekind_type)
            }
            TypeKind::I16 => {
                self.build_int_type_info("i16", 16, true, typekind_enum_id, typekind_type)
            }
            TypeKind::I32 => {
                self.build_int_type_info("i32", 32, true, typekind_enum_id, typekind_type)
            }
            TypeKind::I64 => {
                self.build_int_type_info("i64", 64, true, typekind_enum_id, typekind_type)
            }
            TypeKind::U8 => {
                self.build_int_type_info("u8", 8, false, typekind_enum_id, typekind_type)
            }
            TypeKind::U16 => {
                self.build_int_type_info("u16", 16, false, typekind_enum_id, typekind_type)
            }
            TypeKind::U32 => {
                self.build_int_type_info("u32", 32, false, typekind_enum_id, typekind_type)
            }
            TypeKind::U64 => {
                self.build_int_type_info("u64", 64, false, typekind_enum_id, typekind_type)
            }
            TypeKind::Bool => {
                self.build_simple_type_info("bool", "Bool", typekind_enum_id, typekind_type)
            }
            TypeKind::Unit => {
                self.build_simple_type_info("()", "Unit", typekind_enum_id, typekind_type)
            }
            TypeKind::Never => {
                self.build_simple_type_info("!", "Never", typekind_enum_id, typekind_type)
            }
            TypeKind::Array(array_type_id) => {
                let (elem_ty, len) = self.type_pool.array_def(array_type_id);
                let elem_name = self.type_pool.format_type_name(elem_ty);
                let name = format!("[{elem_name}; {len}]");
                self.build_simple_type_info(&name, "Array", typekind_enum_id, typekind_type)
            }
            _ => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!("@type_info not supported for type '{}'", ty.name()),
                },
                span,
            )),
        }
    }

    /// Build a simple type info struct with just `kind` and `name` fields.
    fn build_simple_type_info(
        &mut self,
        type_name: &str,
        kind_variant_name: &str,
        typekind_enum_id: EnumId,
        typekind_type: Type,
    ) -> CompileResult<ConstValue> {
        let kind_val = ConstValue::EnumVariant {
            enum_id: typekind_enum_id,
            variant_idx: self.typekind_variant_idx(kind_variant_name),
        };
        let name_val = self.alloc_comptime_str(type_name.to_string());

        let fields = vec![
            StructField {
                name: "kind".to_string(),
                ty: typekind_type,

                is_pub: true,
            },
            StructField {
                name: "name".to_string(),
                ty: Type::COMPTIME_STR,

                is_pub: true,
            },
        ];
        let (info_type, _) = self.find_or_create_anon_struct(&fields, &[], &HashMap::default());
        let info_struct_id = match info_type.kind() {
            TypeKind::Struct(id) => id,
            _ => unreachable!(),
        };

        Ok(self.alloc_comptime_struct(info_struct_id, vec![kind_val, name_val]))
    }

    /// Build type info for an integer type (includes `bits` and `is_signed`).
    fn build_int_type_info(
        &mut self,
        type_name: &str,
        bits: i32,
        is_signed: bool,
        typekind_enum_id: EnumId,
        typekind_type: Type,
    ) -> CompileResult<ConstValue> {
        let kind_val = ConstValue::EnumVariant {
            enum_id: typekind_enum_id,
            variant_idx: self.typekind_variant_idx("Int"),
        };
        let name_val = self.alloc_comptime_str(type_name.to_string());

        let fields = vec![
            StructField {
                name: "kind".to_string(),
                ty: typekind_type,

                is_pub: true,
            },
            StructField {
                name: "name".to_string(),
                ty: Type::COMPTIME_STR,

                is_pub: true,
            },
            StructField {
                name: "bits".to_string(),
                ty: Type::I32,

                is_pub: true,
            },
            StructField {
                name: "is_signed".to_string(),
                ty: Type::BOOL,

                is_pub: true,
            },
        ];
        let (info_type, _) = self.find_or_create_anon_struct(&fields, &[], &HashMap::default());
        let info_struct_id = match info_type.kind() {
            TypeKind::Struct(id) => id,
            _ => unreachable!(),
        };

        Ok(self.alloc_comptime_struct(
            info_struct_id,
            vec![
                kind_val,
                name_val,
                ConstValue::Integer(bits as i64),
                ConstValue::Bool(is_signed),
            ],
        ))
    }

    /// Build type info for a struct type (includes `field_count` and `fields` array).
    fn build_struct_type_info(
        &mut self,
        struct_id: StructId,
        typekind_enum_id: EnumId,
        typekind_type: Type,
    ) -> CompileResult<ConstValue> {
        let kind_val = ConstValue::EnumVariant {
            enum_id: typekind_enum_id,
            variant_idx: self.typekind_variant_idx("Struct"),
        };

        // Get struct info
        let struct_def = self.type_pool.struct_def(struct_id);
        let struct_name = struct_def.name.clone();
        let field_defs: Vec<(String, Type)> = struct_def
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty))
            .collect();
        let field_count = field_defs.len() as i32;

        let name_val = self.alloc_comptime_str(struct_name);

        // Create FieldInfo struct type
        let field_info_fields = vec![
            StructField {
                name: "name".to_string(),
                ty: Type::COMPTIME_STR,

                is_pub: true,
            },
            StructField {
                name: "field_type".to_string(),
                ty: Type::COMPTIME_TYPE,

                is_pub: true,
            },
        ];
        let (field_info_type, _) =
            self.find_or_create_anon_struct(&field_info_fields, &[], &HashMap::default());
        let field_info_struct_id = match field_info_type.kind() {
            TypeKind::Struct(id) => id,
            _ => unreachable!(),
        };

        // Create FieldInfo instances for each field
        let mut field_values = Vec::with_capacity(field_defs.len());
        for (fname, ftype) in &field_defs {
            let fname_val = self.alloc_comptime_str(fname.clone());
            let ftype_val = ConstValue::Type(*ftype);
            let field_info =
                self.alloc_comptime_struct(field_info_struct_id, vec![fname_val, ftype_val]);
            field_values.push(field_info);
        }

        // Create the fields array
        let fields_array = self.alloc_comptime_array(field_values);

        // Create the array type for fields: [FieldInfo; N]
        let fields_array_type =
            Type::new_array(self.get_or_create_array_type(field_info_type, field_count as u64));

        // Create the info struct type
        let info_fields = vec![
            StructField {
                name: "kind".to_string(),
                ty: typekind_type,

                is_pub: true,
            },
            StructField {
                name: "name".to_string(),
                ty: Type::COMPTIME_STR,

                is_pub: true,
            },
            StructField {
                name: "field_count".to_string(),
                ty: Type::I32,

                is_pub: true,
            },
            StructField {
                name: "fields".to_string(),
                ty: fields_array_type,

                is_pub: true,
            },
        ];
        let (info_type, _) =
            self.find_or_create_anon_struct(&info_fields, &[], &HashMap::default());
        let info_struct_id = match info_type.kind() {
            TypeKind::Struct(id) => id,
            _ => unreachable!(),
        };

        Ok(self.alloc_comptime_struct(
            info_struct_id,
            vec![
                kind_val,
                name_val,
                ConstValue::Integer(field_count as i64),
                fields_array,
            ],
        ))
    }

    /// Build type info for an enum type (includes `variant_count` and `variants` array).
    fn build_enum_type_info(
        &mut self,
        enum_id: EnumId,
        typekind_enum_id: EnumId,
        typekind_type: Type,
    ) -> CompileResult<ConstValue> {
        let kind_val = ConstValue::EnumVariant {
            enum_id: typekind_enum_id,
            variant_idx: self.typekind_variant_idx("Enum"),
        };

        // Get enum info
        let enum_def = self.type_pool.enum_def(enum_id);
        let enum_name = enum_def.name.clone();
        let variant_defs: Vec<(String, Vec<(String, Type)>)> = enum_def
            .variants
            .iter()
            .map(|v| {
                let vfields: Vec<(String, Type)> = if v.is_struct_variant() {
                    // Struct variant: field names + types
                    v.field_names
                        .iter()
                        .zip(v.fields.iter())
                        .map(|(name, ty)| (name.clone(), *ty))
                        .collect()
                } else {
                    // Unit or tuple variant: just types with positional names
                    v.fields
                        .iter()
                        .enumerate()
                        .map(|(i, ty)| (format!("{i}"), *ty))
                        .collect()
                };
                (v.name.clone(), vfields)
            })
            .collect();
        let variant_count = variant_defs.len() as i32;

        let name_val = self.alloc_comptime_str(enum_name);

        // Create FieldInfo struct type (reuse if already exists)
        let field_info_fields = vec![
            StructField {
                name: "name".to_string(),
                ty: Type::COMPTIME_STR,

                is_pub: true,
            },
            StructField {
                name: "field_type".to_string(),
                ty: Type::COMPTIME_TYPE,

                is_pub: true,
            },
        ];
        let (field_info_type, _) =
            self.find_or_create_anon_struct(&field_info_fields, &[], &HashMap::default());
        let field_info_struct_id = match field_info_type.kind() {
            TypeKind::Struct(id) => id,
            _ => unreachable!(),
        };

        // Create VariantInfo instances
        let mut variant_values = Vec::with_capacity(variant_defs.len());
        for (vname, vfields) in &variant_defs {
            let vname_val = self.alloc_comptime_str(vname.clone());

            // Create FieldInfo array for this variant's fields
            let mut vfield_values = Vec::new();
            for (fname, ftype) in vfields {
                let fname_val = self.alloc_comptime_str(fname.clone());
                let ftype_val = ConstValue::Type(*ftype);
                let field_info =
                    self.alloc_comptime_struct(field_info_struct_id, vec![fname_val, ftype_val]);
                vfield_values.push(field_info);
            }
            let vfields_array = self.alloc_comptime_array(vfield_values);
            let vfield_count = vfields.len() as i32;

            // Create VariantInfo struct type with fields array
            let vfields_array_type = Type::new_array(
                self.get_or_create_array_type(field_info_type, vfields.len() as u64),
            );
            let variant_info_fields = vec![
                StructField {
                    name: "name".to_string(),
                    ty: Type::COMPTIME_STR,

                    is_pub: true,
                },
                StructField {
                    name: "field_count".to_string(),
                    ty: Type::I32,

                    is_pub: true,
                },
                StructField {
                    name: "fields".to_string(),
                    ty: vfields_array_type,

                    is_pub: true,
                },
            ];
            let (variant_info_type, _) =
                self.find_or_create_anon_struct(&variant_info_fields, &[], &HashMap::default());
            let variant_info_struct_id = match variant_info_type.kind() {
                TypeKind::Struct(id) => id,
                _ => unreachable!(),
            };

            let variant_info = self.alloc_comptime_struct(
                variant_info_struct_id,
                vec![
                    vname_val,
                    ConstValue::Integer(vfield_count as i64),
                    vfields_array,
                ],
            );
            variant_values.push(variant_info);
        }

        // Create the variants array
        let variants_array = self.alloc_comptime_array(variant_values);

        // For the array type, we need a VariantInfo type — use one for unit variants (0 fields)
        let empty_fields_array_type =
            Type::new_array(self.get_or_create_array_type(field_info_type, 0));
        let variant_info_fields = vec![
            StructField {
                name: "name".to_string(),
                ty: Type::COMPTIME_STR,

                is_pub: true,
            },
            StructField {
                name: "field_count".to_string(),
                ty: Type::I32,

                is_pub: true,
            },
            StructField {
                name: "fields".to_string(),
                ty: empty_fields_array_type,

                is_pub: true,
            },
        ];
        let (variant_info_type, _) =
            self.find_or_create_anon_struct(&variant_info_fields, &[], &HashMap::default());
        let variants_array_type =
            Type::new_array(self.get_or_create_array_type(variant_info_type, variant_count as u64));

        // Create the info struct type
        let info_fields = vec![
            StructField {
                name: "kind".to_string(),
                ty: typekind_type,

                is_pub: true,
            },
            StructField {
                name: "name".to_string(),
                ty: Type::COMPTIME_STR,

                is_pub: true,
            },
            StructField {
                name: "variant_count".to_string(),
                ty: Type::I32,

                is_pub: true,
            },
            StructField {
                name: "variants".to_string(),
                ty: variants_array_type,

                is_pub: true,
            },
        ];
        let (info_type, _) =
            self.find_or_create_anon_struct(&info_fields, &[], &HashMap::default());
        let info_struct_id = match info_type.kind() {
            TypeKind::Struct(id) => id,
            _ => unreachable!(),
        };

        Ok(self.alloc_comptime_struct(
            info_struct_id,
            vec![
                kind_val,
                name_val,
                ConstValue::Integer(variant_count as i64),
                variants_array,
            ],
        ))
    }

    /// Evaluate a comptime block expression using the stateful interpreter.
    ///
    /// Seeds the local scope from `ctx.comptime_value_vars` (captured comptime
    /// parameters, e.g. `N` in `FixedBuffer(comptime N: i32)`), then delegates
    /// to `evaluate_comptime_inst`.
    pub(crate) fn evaluate_comptime_block(
        &mut self,
        inst_ref: InstRef,
        ctx: &AnalysisContext,
        span: Span,
    ) -> CompileResult<ConstValue> {
        let _span = info_span!("comptime").entered();
        // Reset the step counter and heap for this comptime block evaluation.
        self.comptime_steps_used = 0;
        self.comptime_heap.clear();
        // Seed interpreter locals with any comptime-captured values from the
        // outer analysis context (e.g. `N` in a method of `FixedBuffer(N)`).
        let mut locals = ctx.comptime_value_vars.clone();
        let result = self.evaluate_comptime_inst(inst_ref, &mut locals, ctx, span)?;
        // Control-flow signals escaping the top level are errors.
        // BreakSignal/ContinueSignal mean break/continue outside a loop.
        // ReturnSignal means return outside a function (comptime block is not a function).
        match result {
            ConstValue::BreakSignal | ConstValue::ContinueSignal => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "break/continue outside a loop in comptime block".into(),
                },
                span,
            )),
            ConstValue::ReturnSignal => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "return outside a function in comptime block".into(),
                },
                span,
            )),
            val => Ok(val),
        }
    }

    /// Evaluate a comptime expression at the top level, outside any function body.
    ///
    /// Used during Phase 2.5 const-initializer evaluation, where there is no
    /// enclosing `AnalysisContext`. Builds a minimal stub context (no locals,
    /// no comptime type params) and delegates to [`evaluate_comptime_block`].
    pub(crate) fn evaluate_comptime_top_level(
        &mut self,
        inst_ref: InstRef,
        span: Span,
    ) -> CompileResult<ConstValue> {
        let empty_params: Vec<ParamInfo> = Vec::new();
        let empty_resolved: HashMap<InstRef, Type> = HashMap::default();
        let stub = AnalysisContext {
            locals: HashMap::default(),
            params: &empty_params,
            next_slot: 0,
            loop_depth: 0,
            forbid_break: None,
            checked_depth: 0,
            used_locals: HashSet::default(),
            return_type: Type::UNIT,
            scope_stack: Vec::new(),
            resolved_types: &empty_resolved,
            moved_vars: HashMap::default(),
            warnings: Vec::new(),
            local_string_table: HashMap::default(),
            local_strings: Vec::new(),
            local_bytes: Vec::new(),
            comptime_type_vars: HashMap::default(),
            comptime_value_vars: HashMap::default(),
            referenced_functions: HashSet::default(),
            referenced_methods: HashSet::default(),
            borrow_arg_skip_move: None,
            uninit_handles: HashMap::default(),
            unroll_arm_bindings: HashMap::default(),
        };
        self.evaluate_comptime_block(inst_ref, &stub, span)
    }

    /// Evaluate a type-constructor function body (one returning `type`) using
    /// the stateful interpreter, with the call's type and value bindings seeded
    /// into `comptime_type_overrides` and `locals`.
    ///
    /// Used as a fallback when `try_evaluate_const_with_subst` declines to
    /// evaluate a multi-statement body — e.g. a `comptime if @ownership(T) !=
    /// Ownership::Copy { @compile_error(...) }` guard before the trailing
    /// `struct {…}` literal.
    ///
    /// Returns the produced `Type` on success, or a `CompileError` if the body
    /// cannot be evaluated at compile time (or if a `@compile_error` fires).
    pub(crate) fn evaluate_type_ctor_body(
        &mut self,
        body: InstRef,
        type_subst: &rustc_hash::FxHashMap<Spur, Type>,
        value_subst: &rustc_hash::FxHashMap<Spur, ConstValue>,
        ctx: &AnalysisContext,
        span: Span,
    ) -> CompileResult<Type> {
        // ADR-0082: detect whether this evaluation is the `@lang("vec")`
        // function body. If so, register the resulting `StructId` as a
        // Vec instance for the captured element type, so later sema
        // queries (`as_vec_instance`) can recognize the struct.
        let lang_vec_elem: Option<Type> = self.lang_items.vec_fn().and_then(|sym| {
            let info = self.functions.get(&sym)?;
            if info.body != body {
                return None;
            }
            // The lang-item Vec function has exactly one comptime type
            // parameter (T); read its substitution.
            let param_names = self.param_arena.names(info.params);
            param_names.first().and_then(|n| type_subst.get(n).copied())
        });
        let mut type_overrides: HashMap<Spur, Type> = HashMap::default();
        for (k, v) in type_subst {
            type_overrides.insert(*k, *v);
        }
        let mut locals: HashMap<Spur, ConstValue> = ctx.comptime_value_vars.clone();
        for (k, v) in value_subst {
            locals.insert(*k, *v);
        }

        let saved_overrides = std::mem::replace(&mut self.comptime_type_overrides, type_overrides);
        let prev_steps = self.comptime_steps_used;
        self.comptime_steps_used = 0;
        self.comptime_call_depth += 1;
        let pending_eval_errs_before = self.pending_anon_eval_errors.len();
        let result = self.evaluate_comptime_inst(body, &mut locals, ctx, span);
        self.comptime_call_depth -= 1;
        self.comptime_steps_used = prev_steps;
        self.comptime_type_overrides = saved_overrides;

        // If the inner evaluator pushed a specific anon-eval validation error
        // (e.g. empty struct, duplicate method) and then returned `None`, the
        // generic "expression cannot be known at compile time" error from the
        // delegation chain is unhelpful. Drain the new entries and surface
        // the first one as the call's primary error.
        if self.pending_anon_eval_errors.len() > pending_eval_errs_before {
            let drained: Vec<_> = self
                .pending_anon_eval_errors
                .drain(pending_eval_errs_before..)
                .collect();
            // result may be Ok or Err; either way the validation failure wins.
            let mut iter = drained.into_iter();
            let primary = iter.next().expect("len check above guarantees one entry");
            // Any extras stay in the buffer so they still surface at the end
            // of analysis rather than being silently dropped.
            for extra in iter {
                self.pending_anon_eval_errors.push(extra);
            }
            return Err(primary);
        }

        let final_ty = match result? {
            ConstValue::Type(ty) => ty,
            ConstValue::ReturnSignal => match self.comptime_return_value.take() {
                Some(ConstValue::Type(ty)) => ty,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: "type-constructor function did not return a `type` value"
                                .to_string(),
                        },
                        span,
                    ));
                }
            },
            other => {
                return Err(CompileError::new(
                    ErrorKind::ComptimeEvaluationFailed {
                        reason: format!(
                            "type-constructor function body produced a non-type value: {:?}",
                            other
                        ),
                    },
                    span,
                ));
            }
        };

        // ADR-0082: register the lang-item Vec instance.
        if let Some(elem_ty) = lang_vec_elem
            && let crate::types::TypeKind::Struct(struct_id) = final_ty.kind()
        {
            self.vec_instance_registry.insert(struct_id, elem_ty);
        }
        Ok(final_ty)
    }

    /// Evaluate a comptime expression without clearing the heap.
    ///
    /// This is used by `@field` and other intrinsics inside `comptime_unroll for` bodies
    /// where the heap contains data from the iterable evaluation that must be preserved.
    pub(super) fn evaluate_comptime_expr(
        &mut self,
        inst_ref: InstRef,
        ctx: &AnalysisContext,
        span: Span,
    ) -> CompileResult<ConstValue> {
        let prev_steps = self.comptime_steps_used;
        self.comptime_steps_used = 0;
        let mut locals = ctx.comptime_value_vars.clone();
        let result = self.evaluate_comptime_inst(inst_ref, &mut locals, ctx, span)?;
        self.comptime_steps_used = prev_steps;
        match result {
            ConstValue::BreakSignal | ConstValue::ContinueSignal => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "break/continue outside a loop in comptime expression".into(),
                },
                span,
            )),
            ConstValue::ReturnSignal => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "return outside a function in comptime expression".into(),
                },
                span,
            )),
            val => Ok(val),
        }
    }

    /// Recursively evaluate one RIR instruction in a comptime context.
    ///
    /// `locals` holds variables declared within the current comptime block.
    /// Returns the evaluated `ConstValue`, or a `CompileError` if the
    /// instruction is not compile-time evaluable.
    pub(crate) fn evaluate_comptime_inst(
        &mut self,
        inst_ref: InstRef,
        locals: &mut HashMap<Spur, ConstValue>,
        ctx: &AnalysisContext,
        outer_span: Span,
    ) -> CompileResult<ConstValue> {
        // Clone the instruction data up-front to release the `self.rir` borrow
        // before any recursive calls to `evaluate_comptime_inst`.
        let (inst_span, inst_data) = {
            let inst = self.rir.get(inst_ref);
            (inst.span, inst.data.clone())
        };

        /// Return a "cannot be known at compile time" error at `span`.
        #[inline]
        fn not_const(span: Span) -> CompileError {
            CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "expression contains values that cannot be known at compile time"
                        .into(),
                },
                span,
            )
        }

        /// Return an arithmetic overflow error at `span`.
        #[inline]
        fn overflow(span: Span) -> CompileError {
            CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "arithmetic overflow in comptime evaluation".into(),
                },
                span,
            )
        }

        /// Extract integer from ConstValue, or return not_const error.
        #[inline]
        fn int(v: ConstValue, span: Span) -> CompileResult<i64> {
            v.as_integer().ok_or_else(|| not_const(span))
        }

        /// Extract bool from ConstValue, or return not_const error.
        #[inline]
        fn bool_val(v: ConstValue, span: Span) -> CompileResult<bool> {
            v.as_bool().ok_or_else(|| not_const(span))
        }

        match inst_data {
            // ── Literals ──────────────────────────────────────────────────────
            InstData::IntConst(value) => {
                i64::try_from(value).map(ConstValue::Integer).map_err(|_| {
                    CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: "integer constant too large for comptime evaluation".into(),
                        },
                        inst_span,
                    )
                })
            }

            InstData::BoolConst(value) => Ok(ConstValue::Bool(value)),

            InstData::UnitConst => Ok(ConstValue::Unit),

            InstData::StringConst(spur) => {
                let s = self.interner.resolve(&spur).to_string();
                let idx = self.comptime_heap.len() as u32;
                self.comptime_heap.push(ComptimeHeapItem::String(s));
                Ok(ConstValue::ComptimeStr(idx))
            }

            InstData::Unary { op, operand } => {
                let v = self.evaluate_comptime_inst(operand, locals, ctx, outer_span)?;
                match op {
                    UnaryOp::Neg => Ok(ConstValue::Integer(int(v, inst_span)?.wrapping_neg())),
                    UnaryOp::Not => Ok(ConstValue::Bool(!bool_val(v, inst_span)?)),
                    UnaryOp::BitNot => Ok(ConstValue::Integer(!int(v, inst_span)?)),
                }
            }

            InstData::Bin { op, lhs, rhs } => {
                let lv = self.evaluate_comptime_inst(lhs, locals, ctx, outer_span)?;
                let rv = self.evaluate_comptime_inst(rhs, locals, ctx, outer_span)?;
                let div_zero = |what: &str| {
                    CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!("{} by zero in comptime evaluation", what),
                        },
                        inst_span,
                    )
                };
                let shift_oob = || {
                    CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: "shift amount out of range in comptime evaluation".into(),
                        },
                        inst_span,
                    )
                };
                match op {
                    BinOp::Add => Ok(ConstValue::Integer(
                        int(lv, inst_span)?.wrapping_add(int(rv, inst_span)?),
                    )),
                    BinOp::Sub => Ok(ConstValue::Integer(
                        int(lv, inst_span)?.wrapping_sub(int(rv, inst_span)?),
                    )),
                    BinOp::Mul => Ok(ConstValue::Integer(
                        int(lv, inst_span)?.wrapping_mul(int(rv, inst_span)?),
                    )),
                    BinOp::Div => {
                        let r = int(rv, inst_span)?;
                        if r == 0 {
                            return Err(div_zero("division"));
                        }
                        Ok(ConstValue::Integer(int(lv, inst_span)?.wrapping_div(r)))
                    }
                    BinOp::Mod => {
                        let r = int(rv, inst_span)?;
                        if r == 0 {
                            return Err(div_zero("modulo"));
                        }
                        Ok(ConstValue::Integer(int(lv, inst_span)?.wrapping_rem(r)))
                    }
                    BinOp::Eq | BinOp::Ne => {
                        let eq = match (lv, rv) {
                            (ConstValue::Integer(a), ConstValue::Integer(b)) => a == b,
                            (ConstValue::Bool(a), ConstValue::Bool(b)) => a == b,
                            (ConstValue::ComptimeStr(a), ConstValue::ComptimeStr(b)) => {
                                let sa = self.resolve_comptime_str(a, inst_span)?;
                                let sb = self.resolve_comptime_str(b, inst_span)?;
                                sa == sb
                            }
                            (
                                ConstValue::EnumVariant {
                                    enum_id: ae,
                                    variant_idx: av,
                                },
                                ConstValue::EnumVariant {
                                    enum_id: be,
                                    variant_idx: bv,
                                },
                            ) => ae == be && av == bv,
                            _ => return Err(not_const(inst_span)),
                        };
                        Ok(ConstValue::Bool(if matches!(op, BinOp::Eq) {
                            eq
                        } else {
                            !eq
                        }))
                    }
                    BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                        let ord = match (lv, rv) {
                            (ConstValue::Integer(a), ConstValue::Integer(b)) => a.cmp(&b),
                            (ConstValue::ComptimeStr(a), ConstValue::ComptimeStr(b)) => {
                                let sa = self.resolve_comptime_str(a, inst_span)?;
                                let sb = self.resolve_comptime_str(b, inst_span)?;
                                sa.cmp(sb)
                            }
                            _ => return Err(not_const(inst_span)),
                        };
                        use std::cmp::Ordering::*;
                        let result = matches!(
                            (op, ord),
                            (BinOp::Lt, Less)
                                | (BinOp::Gt, Greater)
                                | (BinOp::Le, Less | Equal)
                                | (BinOp::Ge, Greater | Equal)
                        );
                        Ok(ConstValue::Bool(result))
                    }
                    BinOp::And => Ok(ConstValue::Bool(
                        bool_val(lv, inst_span)? && bool_val(rv, inst_span)?,
                    )),
                    BinOp::Or => Ok(ConstValue::Bool(
                        bool_val(lv, inst_span)? || bool_val(rv, inst_span)?,
                    )),
                    BinOp::BitAnd => Ok(ConstValue::Integer(
                        int(lv, inst_span)? & int(rv, inst_span)?,
                    )),
                    BinOp::BitOr => Ok(ConstValue::Integer(
                        int(lv, inst_span)? | int(rv, inst_span)?,
                    )),
                    BinOp::BitXor => Ok(ConstValue::Integer(
                        int(lv, inst_span)? ^ int(rv, inst_span)?,
                    )),
                    BinOp::Shl => {
                        let r = int(rv, inst_span)?;
                        if !(0..64).contains(&r) {
                            return Err(shift_oob());
                        }
                        Ok(ConstValue::Integer(int(lv, inst_span)? << r))
                    }
                    BinOp::Shr => {
                        let r = int(rv, inst_span)?;
                        if !(0..64).contains(&r) {
                            return Err(shift_oob());
                        }
                        Ok(ConstValue::Integer(int(lv, inst_span)? >> r))
                    }
                }
            }

            // ── Block: iterate instructions, return last value ─────────────────
            InstData::Block { extra_start, len } => {
                // Collect into owned Vec to release the `self.rir` borrow before
                // the loop body calls `evaluate_comptime_inst` recursively.
                let raw_refs: Vec<u32> = self.rir.get_extra(extra_start, len).to_vec();
                let mut last_val = ConstValue::Unit;
                for raw_ref in raw_refs {
                    last_val = self.evaluate_comptime_inst(
                        InstRef::from_raw(raw_ref),
                        locals,
                        ctx,
                        outer_span,
                    )?;
                    // Propagate control-flow signals immediately — don't execute
                    // remaining statements after a break, continue, or return.
                    if matches!(
                        last_val,
                        ConstValue::BreakSignal
                            | ConstValue::ContinueSignal
                            | ConstValue::ReturnSignal
                    ) {
                        return Ok(last_val);
                    }
                }
                Ok(last_val)
            }

            // ── Variable declaration ──────────────────────────────────────────
            InstData::Alloc { name, init, .. } => {
                let val = self.evaluate_comptime_inst(init, locals, ctx, outer_span)?;
                if let Some(name_sym) = name {
                    locals.insert(name_sym, val);
                }
                Ok(ConstValue::Unit)
            }

            // ── Variable reference ────────────────────────────────────────────
            InstData::VarRef { name } => {
                // 1. Locals declared within this comptime block (or seeded from outer captures).
                if let Some(&val) = locals.get(&name) {
                    return Ok(val);
                }
                // 2. Comptime type overrides (type params bound during generic function calls).
                if let Some(&ty) = self.comptime_type_overrides.get(&name) {
                    return Ok(ConstValue::Type(ty));
                }
                // 3. Comptime type variables from the outer analysis context
                //    (e.g. `let P = make_point()` in the enclosing function).
                if let Some(&ty) = ctx.comptime_type_vars.get(&name) {
                    return Ok(ConstValue::Type(ty));
                }
                // 4. Built-in type names used as values (e.g. `i32` in `identity(i32, 42)`).
                let name_str = self.interner.resolve(&name).to_string();
                let builtin_ty = match name_str.as_str() {
                    "i8" => Some(Type::I8),
                    "i16" => Some(Type::I16),
                    "i32" => Some(Type::I32),
                    "i64" => Some(Type::I64),
                    "u8" => Some(Type::U8),
                    "u16" => Some(Type::U16),
                    "u32" => Some(Type::U32),
                    "u64" => Some(Type::U64),
                    "isize" => Some(Type::ISIZE),
                    "usize" => Some(Type::USIZE),
                    "f16" => Some(Type::F16),
                    "f32" => Some(Type::F32),
                    "f64" => Some(Type::F64),
                    "bool" => Some(Type::BOOL),
                    "char" => Some(Type::CHAR),
                    "()" => Some(Type::UNIT),
                    "!" => Some(Type::NEVER),
                    _ => None,
                };
                if let Some(ty) = builtin_ty {
                    return Ok(ConstValue::Type(ty));
                }
                // 5. User-defined struct/enum types used as values.
                if let Some(&struct_id) = self.structs.get(&name) {
                    return Ok(ConstValue::Type(Type::new_struct(struct_id)));
                }
                if let Some(&enum_id) = self.enums.get(&name) {
                    return Ok(ConstValue::Type(Type::new_enum(enum_id)));
                }
                // 6. Not a known comptime value — must be a runtime variable.
                Err(not_const(inst_span))
            }

            // ── Assignment ────────────────────────────────────────────────────
            InstData::Assign { name, value } => {
                let val = self.evaluate_comptime_inst(value, locals, ctx, outer_span)?;
                locals.insert(name, val);
                Ok(ConstValue::Unit)
            }

            // ── Branch (if/else) ──────────────────────────────────────────────
            InstData::Branch {
                cond,
                then_block,
                else_block,
                is_comptime: _,
            } => {
                let cond_val = self.evaluate_comptime_inst(cond, locals, ctx, outer_span)?;
                match cond_val {
                    ConstValue::Bool(true) => {
                        self.evaluate_comptime_inst(then_block, locals, ctx, outer_span)
                    }
                    ConstValue::Bool(false) => {
                        if let Some(else_ref) = else_block {
                            self.evaluate_comptime_inst(else_ref, locals, ctx, outer_span)
                        } else {
                            Ok(ConstValue::Unit)
                        }
                    }
                    _ => Err(not_const(inst_span)),
                }
            }

            // ── Nested comptime ───────────────────────────────────────────────
            InstData::Comptime { expr } => {
                self.evaluate_comptime_inst(expr, locals, ctx, outer_span)
            }

            // ── Declarations are no-ops in comptime context ───────────────────
            InstData::FnDecl { .. }
            | InstData::DropFnDecl { .. }
            | InstData::ConstDecl { .. }
            | InstData::StructDecl { .. }
            | InstData::EnumDecl { .. } => Ok(ConstValue::Unit),

            // ── Type-related: delegate to existing evaluator ──────────────────
            // AnonStructType and TypeConst need the full try_evaluate_const
            // logic (type registry lookups, structural equality, etc.).
            // Route through the substitution-aware variant so the active
            // comptime call's type/value bindings (`comptime_type_overrides`
            // plus the current `locals`) feed both field-type resolution and
            // method registration — otherwise a multi-statement type-ctor
            // body (`comptime if … ; struct {…}`) would fail to register
            // methods that reference `T` or capture comptime values.
            InstData::AnonStructType { .. }
            | InstData::AnonEnumType { .. }
            | InstData::TypeConst { .. } => {
                let type_subst = self.comptime_type_overrides.clone();
                let value_subst = locals.clone();
                self.try_evaluate_const_with_subst(inst_ref, &type_subst, &value_subst)
                    .ok_or_else(|| not_const(inst_span))
            }

            // ── While loop ────────────────────────────────────────────────────
            // `while cond { body }` — evaluates until condition is false.
            InstData::Loop { cond, body } => {
                const COMPTIME_MAX_STEPS: u64 = 1_000_000;
                loop {
                    let cond_val = self.evaluate_comptime_inst(cond, locals, ctx, outer_span)?;
                    if !bool_val(cond_val, inst_span)? {
                        break;
                    }
                    self.comptime_steps_used += 1;
                    if self.comptime_steps_used > COMPTIME_MAX_STEPS {
                        return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: format!(
                                    "comptime evaluation exceeded step budget of {} iterations",
                                    COMPTIME_MAX_STEPS
                                ),
                            },
                            inst_span,
                        ));
                    }
                    match self.evaluate_comptime_inst(body, locals, ctx, outer_span)? {
                        ConstValue::BreakSignal => break,
                        ConstValue::ContinueSignal => continue,
                        _ => {}
                    }
                }
                Ok(ConstValue::Unit)
            }

            // ── For-in loop ──────────────────────────────────────────────────
            // Not supported in comptime context (desugared to while at runtime).
            InstData::For { .. } => Err(not_const(inst_span)),

            // ── Infinite loop ─────────────────────────────────────────────────
            // `loop { body }` — runs until a break (or step budget exceeded).
            InstData::InfiniteLoop { body } => {
                const COMPTIME_MAX_STEPS: u64 = 1_000_000;
                loop {
                    self.comptime_steps_used += 1;
                    if self.comptime_steps_used > COMPTIME_MAX_STEPS {
                        return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: format!(
                                    "comptime evaluation exceeded step budget of {} iterations",
                                    COMPTIME_MAX_STEPS
                                ),
                            },
                            inst_span,
                        ));
                    }
                    match self.evaluate_comptime_inst(body, locals, ctx, outer_span)? {
                        ConstValue::BreakSignal => break,
                        ConstValue::ContinueSignal => continue,
                        _ => {}
                    }
                }
                Ok(ConstValue::Unit)
            }

            // ── Break / Continue ──────────────────────────────────────────────
            InstData::Break => Ok(ConstValue::BreakSignal),
            InstData::Continue => Ok(ConstValue::ContinueSignal),

            // ── Return ────────────────────────────────────────────────────────
            // `return expr` or bare `return` inside a comptime function.
            // Stores the return value in a side channel then signals the Call handler.
            InstData::Ret(opt_ref) => {
                let return_val = match opt_ref {
                    Some(val_ref) => {
                        self.evaluate_comptime_inst(val_ref, locals, ctx, outer_span)?
                    }
                    None => ConstValue::Unit,
                };
                self.comptime_return_value = Some(return_val);
                Ok(ConstValue::ReturnSignal)
            }

            // ── Function call ─────────────────────────────────────────────────
            // Evaluate the callee's body with the arguments bound as locals.
            InstData::Call {
                name,
                args_start,
                args_len,
            } => {
                const COMPTIME_CALL_DEPTH_LIMIT: u32 = 64;

                // Look up the function in the function table.
                let fn_info = match self.functions.get(&name) {
                    Some(info) => *info,
                    None => return Err(not_const(inst_span)),
                };

                // Evaluate all arguments before entering the callee frame.
                let call_args = self.rir.get_call_args(args_start, args_len);
                let mut arg_values = Vec::with_capacity(call_args.len());
                for call_arg in &call_args {
                    let val =
                        self.evaluate_comptime_inst(call_arg.value, locals, ctx, outer_span)?;
                    arg_values.push(val);
                }

                // For generic functions, extract type parameter bindings from
                // comptime arguments and set them as type overrides so that
                // struct/enum resolution inside the callee body can find them.
                let param_comptime = self.param_arena.comptime(fn_info.params).to_vec();
                let param_names = self.param_arena.names(fn_info.params).to_vec();
                let mut type_overrides: HashMap<Spur, Type> = HashMap::default();
                if fn_info.is_generic {
                    for (i, is_comptime) in param_comptime.iter().enumerate() {
                        if *is_comptime && let Some(ConstValue::Type(ty)) = arg_values.get(i) {
                            type_overrides.insert(param_names[i], *ty);
                        }
                    }
                }

                // Enforce call stack depth limit to prevent infinite recursion.
                if self.comptime_call_depth >= COMPTIME_CALL_DEPTH_LIMIT {
                    return Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "comptime call stack depth exceeded {} levels (possible infinite recursion)",
                                COMPTIME_CALL_DEPTH_LIMIT
                            ),
                        },
                        inst_span,
                    ));
                }

                // Bind non-comptime parameters to argument values in a fresh call frame.
                // Comptime (type) parameters are not bound as locals — they are
                // available through comptime_type_overrides.
                let mut call_locals: HashMap<Spur, ConstValue> = {
                    let mut m = HashMap::default();
                    m.reserve(param_names.len());
                    m
                };
                for (i, (param_name, arg_val)) in
                    param_names.iter().zip(arg_values.iter()).enumerate()
                {
                    if !param_comptime.get(i).copied().unwrap_or(false) {
                        call_locals.insert(*param_name, *arg_val);
                    }
                }

                // Push type overrides for the duration of this call.
                let saved_overrides =
                    std::mem::replace(&mut self.comptime_type_overrides, type_overrides);

                // Execute the callee body.
                self.comptime_call_depth += 1;
                let body_result =
                    self.evaluate_comptime_inst(fn_info.body, &mut call_locals, ctx, outer_span);
                self.comptime_call_depth -= 1;

                // Restore previous type overrides.
                self.comptime_type_overrides = saved_overrides;

                let body_result = body_result?;

                // Consume any return signal; fall through on plain values.
                match body_result {
                    ConstValue::ReturnSignal => {
                        // `return val` was executed — take the stored value.
                        self.comptime_return_value.take().ok_or_else(|| {
                            CompileError::new(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: "comptime return signal missing its value".into(),
                                },
                                inst_span,
                            )
                        })
                    }
                    ConstValue::BreakSignal | ConstValue::ContinueSignal => {
                        // break/continue escaped a function body — syntax error in Gruel.
                        Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: "break/continue outside a loop in comptime function".into(),
                            },
                            inst_span,
                        ))
                    }
                    val => Ok(val),
                }
            }

            // ── Struct construction ───────────────────────────────────────────
            InstData::StructInit {
                module,
                type_name,
                fields_start,
                fields_len,
            } => {
                // Module-qualified struct literals are not supported in comptime.
                if module.is_some() {
                    return Err(not_const(inst_span));
                }

                // Resolve the struct type by name (also checks comptime type overrides).
                let struct_id = match self.resolve_comptime_struct(type_name, ctx) {
                    Some(id) => id,
                    None => return Err(not_const(inst_span)),
                };

                // Get the struct definition to know field declaration order.
                let struct_def = self.type_pool.struct_def(struct_id);
                let field_count = struct_def.fields.len();

                // Build a map from field name string to declaration index.
                let field_index_map: rustc_hash::FxHashMap<String, usize> = struct_def
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, f)| (f.name.clone(), i))
                    .collect();

                // Retrieve field initializers from the RIR (may be in any source order).
                let field_inits = self.rir.get_field_inits(fields_start, fields_len);

                // Evaluate each field expression and place it in declaration order.
                let mut field_values = vec![ConstValue::Unit; field_count];
                for (field_name_sym, field_value_ref) in &field_inits {
                    let field_name = self.interner.resolve(field_name_sym).to_string();
                    let idx = match field_index_map.get(&field_name) {
                        Some(&i) => i,
                        None => return Err(not_const(inst_span)),
                    };
                    let val =
                        self.evaluate_comptime_inst(*field_value_ref, locals, ctx, outer_span)?;
                    field_values[idx] = val;
                }

                // Allocate a new heap item and return its index.
                let heap_idx = self.comptime_heap.len() as u32;
                self.comptime_heap.push(ComptimeHeapItem::Struct {
                    struct_id,
                    fields: field_values,
                });
                Ok(ConstValue::Struct(heap_idx))
            }

            // ── Field access ──────────────────────────────────────────────────
            InstData::FieldGet { base, field } => {
                let base_val = self.evaluate_comptime_inst(base, locals, ctx, outer_span)?;
                match base_val {
                    ConstValue::Struct(heap_idx) => {
                        // Clone data out to release the heap borrow before calling struct_def.
                        let (struct_id, fields) = match &self.comptime_heap[heap_idx as usize] {
                            ComptimeHeapItem::Struct { struct_id, fields } => {
                                (*struct_id, fields.clone())
                            }
                            _ => return Err(not_const(inst_span)),
                        };
                        let struct_def = self.type_pool.struct_def(struct_id);
                        let field_name = self.interner.resolve(&field);
                        let (field_idx, _) =
                            struct_def.find_field(field_name).ok_or_else(|| {
                                CompileError::new(
                                    ErrorKind::ComptimeEvaluationFailed {
                                        reason: format!(
                                            "unknown field '{}' in comptime struct",
                                            field_name
                                        ),
                                    },
                                    inst_span,
                                )
                            })?;
                        Ok(fields[field_idx])
                    }
                    _ => Err(not_const(inst_span)),
                }
            }

            // ── Array construction ────────────────────────────────────────────
            InstData::ArrayInit {
                elems_start,
                elems_len,
            } => {
                let elem_refs = self.rir.get_inst_refs(elems_start, elems_len);
                let mut elem_values = Vec::with_capacity(elem_refs.len());
                for elem_ref in &elem_refs {
                    let val = self.evaluate_comptime_inst(*elem_ref, locals, ctx, outer_span)?;
                    elem_values.push(val);
                }
                let heap_idx = self.comptime_heap.len() as u32;
                self.comptime_heap
                    .push(ComptimeHeapItem::Array(elem_values));
                Ok(ConstValue::Array(heap_idx))
            }

            // ── Array index read ──────────────────────────────────────────────
            InstData::IndexGet { base, index } => {
                let base_val = self.evaluate_comptime_inst(base, locals, ctx, outer_span)?;
                let index_val = self.evaluate_comptime_inst(index, locals, ctx, outer_span)?;
                match base_val {
                    ConstValue::Array(heap_idx) => {
                        let idx = int(index_val, inst_span)?;
                        // Clone elements to release heap borrow before error construction.
                        let elems = match &self.comptime_heap[heap_idx as usize] {
                            ComptimeHeapItem::Array(elems) => elems.clone(),
                            _ => return Err(not_const(inst_span)),
                        };
                        if idx < 0 || idx as usize >= elems.len() {
                            return Err(CompileError::new(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: format!(
                                        "array index {} out of bounds (length {})",
                                        idx,
                                        elems.len()
                                    ),
                                },
                                inst_span,
                            ));
                        }
                        Ok(elems[idx as usize])
                    }
                    _ => Err(not_const(inst_span)),
                }
            }

            // ── Field mutation ────────────────────────────────────────────────
            InstData::FieldSet { base, field, value } => {
                // base must be a VarRef to a local holding a ConstValue::Struct(heap_idx).
                let var_name = match &self.rir.get(base).data {
                    InstData::VarRef { name } => *name,
                    _ => return Err(not_const(inst_span)),
                };
                let heap_idx = match locals.get(&var_name) {
                    Some(ConstValue::Struct(idx)) => *idx,
                    _ => return Err(not_const(inst_span)),
                };
                let val = self.evaluate_comptime_inst(value, locals, ctx, outer_span)?;
                // Resolve field index from struct definition.
                let struct_id = match &self.comptime_heap[heap_idx as usize] {
                    ComptimeHeapItem::Struct { struct_id, .. } => *struct_id,
                    _ => return Err(not_const(inst_span)),
                };
                let struct_def = self.type_pool.struct_def(struct_id);
                let field_name = self.interner.resolve(&field);
                let (field_idx, _) = struct_def.find_field(field_name).ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!("unknown field '{}' in comptime struct", field_name),
                        },
                        inst_span,
                    )
                })?;
                // Mutate the heap item in place.
                match &mut self.comptime_heap[heap_idx as usize] {
                    ComptimeHeapItem::Struct { fields, .. } => {
                        fields[field_idx] = val;
                    }
                    _ => return Err(not_const(inst_span)),
                }
                Ok(ConstValue::Unit)
            }

            // ── Array element mutation ───────────────────────────────────────────
            InstData::IndexSet { base, index, value } => {
                // base must be a VarRef to a local holding a ConstValue::Array(heap_idx).
                let var_name = match &self.rir.get(base).data {
                    InstData::VarRef { name } => *name,
                    _ => return Err(not_const(inst_span)),
                };
                let heap_idx = match locals.get(&var_name) {
                    Some(ConstValue::Array(idx)) => *idx,
                    _ => return Err(not_const(inst_span)),
                };
                let idx = int(
                    self.evaluate_comptime_inst(index, locals, ctx, outer_span)?,
                    inst_span,
                )?;
                let val = self.evaluate_comptime_inst(value, locals, ctx, outer_span)?;
                // Bounds check and mutate.
                let len = match &self.comptime_heap[heap_idx as usize] {
                    ComptimeHeapItem::Array(elems) => elems.len(),
                    _ => return Err(not_const(inst_span)),
                };
                if idx < 0 || idx as usize >= len {
                    return Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!("array index {} out of bounds (length {})", idx, len),
                        },
                        inst_span,
                    ));
                }
                match &mut self.comptime_heap[heap_idx as usize] {
                    ComptimeHeapItem::Array(elems) => {
                        elems[idx as usize] = val;
                    }
                    _ => return Err(not_const(inst_span)),
                }
                Ok(ConstValue::Unit)
            }

            // ── Unit enum variant ──────────────────────────────────────────────
            InstData::EnumVariant {
                module: _,
                type_name,
                variant,
            } => {
                // Resolve enum ID — check direct enums, then comptime type vars.
                let enum_id = if let Some(&id) = self.enums.get(&type_name) {
                    id
                } else if let Some(&ty) = ctx.comptime_type_vars.get(&type_name) {
                    match ty.kind() {
                        TypeKind::Enum(id) => id,
                        _ => return Err(not_const(inst_span)),
                    }
                } else {
                    return Err(not_const(inst_span));
                };
                let enum_def = self.type_pool.enum_def(enum_id);
                let variant_name = self.interner.resolve(&variant);
                let variant_idx = enum_def
                    .find_variant(variant_name)
                    .ok_or_else(|| not_const(inst_span))? as u32;
                Ok(ConstValue::EnumVariant {
                    enum_id,
                    variant_idx,
                })
            }

            // ── Struct-style enum variant ─────────────────────────────────────────
            InstData::EnumStructVariant {
                module: _,
                type_name,
                variant,
                fields_start,
                fields_len,
            } => {
                // Resolve enum ID.
                let enum_id = if let Some(&id) = self.enums.get(&type_name) {
                    id
                } else if let Some(&ty) = ctx.comptime_type_vars.get(&type_name) {
                    match ty.kind() {
                        TypeKind::Enum(id) => id,
                        _ => return Err(not_const(inst_span)),
                    }
                } else {
                    return Err(not_const(inst_span));
                };
                let enum_def = self.type_pool.enum_def(enum_id);
                let variant_name = self.interner.resolve(&variant);
                let variant_idx = enum_def
                    .find_variant(variant_name)
                    .ok_or_else(|| not_const(inst_span))? as u32;
                let variant_def = &enum_def.variants[variant_idx as usize];

                // Get field initializers and resolve to declaration order.
                let field_inits = self.rir.get_field_inits(fields_start, fields_len);
                let mut field_values = vec![ConstValue::Unit; variant_def.fields.len()];
                for (init_field_name, field_value_ref) in &field_inits {
                    let field_name_str = self.interner.resolve(init_field_name);
                    let field_idx = variant_def
                        .find_field(field_name_str)
                        .ok_or_else(|| not_const(inst_span))?;
                    let val =
                        self.evaluate_comptime_inst(*field_value_ref, locals, ctx, outer_span)?;
                    field_values[field_idx] = val;
                }

                let heap_idx = self.comptime_heap.len() as u32;
                self.comptime_heap
                    .push(ComptimeHeapItem::EnumStruct(field_values));
                Ok(ConstValue::EnumStruct {
                    enum_id,
                    variant_idx,
                    heap_idx,
                })
            }

            // ── Tuple data enum variant (via AssocFnCall) ─────────────────────────
            InstData::AssocFnCall {
                type_name,
                function,
                args_start,
                args_len,
            } => {
                // Check if this is an enum data variant construction.
                let enum_id = if let Some(&id) = self.enums.get(&type_name) {
                    Some(id)
                } else if let Some(&ty) = ctx.comptime_type_vars.get(&type_name) {
                    match ty.kind() {
                        TypeKind::Enum(id) => Some(id),
                        _ => None,
                    }
                } else {
                    None
                };

                if let Some(enum_id) = enum_id {
                    let enum_def = self.type_pool.enum_def(enum_id);
                    let variant_name = self.interner.resolve(&function);
                    if let Some(variant_idx) = enum_def.find_variant(variant_name) {
                        let variant_def = &enum_def.variants[variant_idx];
                        if variant_def.has_data() && !variant_def.is_struct_variant() {
                            // Tuple data variant: evaluate arguments.
                            let call_args = self.rir.get_call_args(args_start, args_len);
                            let mut field_values = Vec::with_capacity(variant_def.fields.len());
                            for arg in &call_args {
                                let val = self
                                    .evaluate_comptime_inst(arg.value, locals, ctx, outer_span)?;
                                field_values.push(val);
                            }
                            let heap_idx = self.comptime_heap.len() as u32;
                            self.comptime_heap
                                .push(ComptimeHeapItem::EnumData(field_values));
                            return Ok(ConstValue::EnumData {
                                enum_id,
                                variant_idx: variant_idx as u32,
                                heap_idx,
                            });
                        }
                    }
                }

                // Not an enum data variant — unsupported in comptime.
                Err(not_const(inst_span))
            }

            // ── Pattern matching ───────────────────────────────────────────────
            InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            } => {
                let scrut_val = self.evaluate_comptime_inst(scrutinee, locals, ctx, outer_span)?;
                let arms = self.rir.get_match_arms(arms_start, arms_len);

                for (pattern, body) in &arms {
                    match pattern {
                        RirPattern::Wildcard(_) => {
                            // Always matches — evaluate body directly.
                            return self.evaluate_comptime_inst(*body, locals, ctx, outer_span);
                        }
                        RirPattern::Int(n, _) => {
                            if let ConstValue::Integer(val) = scrut_val
                                && val == *n
                            {
                                return self.evaluate_comptime_inst(*body, locals, ctx, outer_span);
                            }
                        }
                        RirPattern::Bool(b, _) => {
                            if let ConstValue::Bool(val) = scrut_val
                                && val == *b
                            {
                                return self.evaluate_comptime_inst(*body, locals, ctx, outer_span);
                            }
                        }
                        RirPattern::Path {
                            type_name, variant, ..
                        } => {
                            // Match unit enum variant by name.
                            let pat_enum_id = self.resolve_comptime_enum(*type_name, ctx);
                            if let Some(pat_enum_id) = pat_enum_id {
                                let enum_def = self.type_pool.enum_def(pat_enum_id);
                                let variant_name = self.interner.resolve(variant);
                                if let Some(pat_variant_idx) = enum_def.find_variant(variant_name) {
                                    let matches = match scrut_val {
                                        ConstValue::EnumVariant {
                                            enum_id,
                                            variant_idx,
                                        } => {
                                            enum_id == pat_enum_id
                                                && variant_idx == pat_variant_idx as u32
                                        }
                                        ConstValue::EnumData {
                                            enum_id,
                                            variant_idx,
                                            ..
                                        } => {
                                            enum_id == pat_enum_id
                                                && variant_idx == pat_variant_idx as u32
                                        }
                                        ConstValue::EnumStruct {
                                            enum_id,
                                            variant_idx,
                                            ..
                                        } => {
                                            enum_id == pat_enum_id
                                                && variant_idx == pat_variant_idx as u32
                                        }
                                        _ => false,
                                    };
                                    if matches {
                                        return self.evaluate_comptime_inst(
                                            *body, locals, ctx, outer_span,
                                        );
                                    }
                                }
                            }
                        }
                        RirPattern::DataVariant {
                            type_name,
                            variant,
                            bindings,
                            ..
                        } => {
                            // Match tuple data variant and bind fields.
                            let pat_enum_id = self.resolve_comptime_enum(*type_name, ctx);
                            if let Some(pat_enum_id) = pat_enum_id {
                                let enum_def = self.type_pool.enum_def(pat_enum_id);
                                let variant_name = self.interner.resolve(variant);
                                if let Some(pat_variant_idx) = enum_def.find_variant(variant_name) {
                                    let (matches, heap_idx_opt) = match scrut_val {
                                        ConstValue::EnumData {
                                            enum_id,
                                            variant_idx,
                                            heap_idx,
                                        } if enum_id == pat_enum_id
                                            && variant_idx == pat_variant_idx as u32 =>
                                        {
                                            (true, Some(heap_idx))
                                        }
                                        _ => (false, None),
                                    };
                                    if matches {
                                        // Bind fields into locals.
                                        if let Some(heap_idx) = heap_idx_opt {
                                            let field_values =
                                                match &self.comptime_heap[heap_idx as usize] {
                                                    ComptimeHeapItem::EnumData(fields) => {
                                                        fields.clone()
                                                    }
                                                    _ => return Err(not_const(inst_span)),
                                                };
                                            for (i, binding) in bindings.iter().enumerate() {
                                                if !binding.is_wildcard
                                                    && let Some(name) = binding.name
                                                {
                                                    locals.insert(name, field_values[i]);
                                                }
                                            }
                                        }
                                        return self.evaluate_comptime_inst(
                                            *body, locals, ctx, outer_span,
                                        );
                                    }
                                }
                            }
                        }
                        RirPattern::StructVariant {
                            type_name,
                            variant,
                            field_bindings,
                            ..
                        } => {
                            // Match struct variant and bind named fields.
                            let pat_enum_id = self.resolve_comptime_enum(*type_name, ctx);
                            if let Some(pat_enum_id) = pat_enum_id {
                                let enum_def = self.type_pool.enum_def(pat_enum_id);
                                let variant_name = self.interner.resolve(variant);
                                if let Some(pat_variant_idx) = enum_def.find_variant(variant_name) {
                                    let (matches, heap_idx_opt) = match scrut_val {
                                        ConstValue::EnumStruct {
                                            enum_id,
                                            variant_idx,
                                            heap_idx,
                                        } if enum_id == pat_enum_id
                                            && variant_idx == pat_variant_idx as u32 =>
                                        {
                                            (true, Some(heap_idx))
                                        }
                                        _ => (false, None),
                                    };
                                    if matches {
                                        if let Some(heap_idx) = heap_idx_opt {
                                            let field_values =
                                                match &self.comptime_heap[heap_idx as usize] {
                                                    ComptimeHeapItem::EnumStruct(fields) => {
                                                        fields.clone()
                                                    }
                                                    _ => return Err(not_const(inst_span)),
                                                };
                                            let variant_def = &enum_def.variants[pat_variant_idx];
                                            for fb in field_bindings {
                                                if !fb.binding.is_wildcard
                                                    && let Some(name) = fb.binding.name
                                                {
                                                    let field_name_str =
                                                        self.interner.resolve(&fb.field_name);
                                                    let field_idx = match variant_def
                                                        .find_field(field_name_str)
                                                    {
                                                        Some(idx) => idx,
                                                        None => return Err(not_const(inst_span)),
                                                    };
                                                    locals.insert(name, field_values[field_idx]);
                                                }
                                            }
                                        }
                                        return self.evaluate_comptime_inst(
                                            *body, locals, ctx, outer_span,
                                        );
                                    }
                                }
                            }
                        }
                        // ADR-0051 Phase 4a: Ident / Tuple / Struct are not yet
                        // produced by astgen, so comptime match evaluation does
                        // not need to handle them yet. Phase 4b turns astgen on
                        // and fills these in.
                        RirPattern::Ident { .. }
                        | RirPattern::Tuple { .. }
                        | RirPattern::Struct { .. } => {
                            unreachable!(
                                "RirPattern::Ident/Tuple/Struct are not produced by astgen in \
                                 ADR-0051 Phase 4a"
                            )
                        }
                        // ADR-0079 Phase 3: an unroll-arm template
                        // shouldn't reach the comptime evaluator —
                        // sema's `expand_unroll_arms` runs before
                        // any analysis sees it. If we hit it here,
                        // something pre-analysis comptime-evaluated
                        // the match.
                        RirPattern::ComptimeUnrollArm { .. } => {
                            return Err(CompileError::new(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason:
                                        "comptime_unroll for arm cannot be evaluated at comptime"
                                            .into(),
                                },
                                outer_span,
                            ));
                        }
                    }
                }

                // No arm matched — should not happen after exhaustiveness checking.
                Err(CompileError::new(
                    ErrorKind::ComptimeEvaluationFailed {
                        reason: "no match arm matched in comptime evaluation".into(),
                    },
                    inst_span,
                ))
            }

            // ── Struct destructuring ─────────────────────────────────────────
            InstData::StructDestructure {
                type_name,
                fields_start,
                fields_len,
                init,
            } => {
                // Evaluate the initializer to a struct value.
                let init_val = self.evaluate_comptime_inst(init, locals, ctx, outer_span)?;
                let heap_idx = match init_val {
                    ConstValue::Struct(idx) => idx,
                    _ => return Err(not_const(inst_span)),
                };

                // Resolve the struct type.
                let struct_id = match self.resolve_comptime_struct(type_name, ctx) {
                    Some(id) => id,
                    None => return Err(not_const(inst_span)),
                };

                // Get field values from the heap.
                let field_values = match &self.comptime_heap[heap_idx as usize] {
                    ComptimeHeapItem::Struct { fields, .. } => fields.clone(),
                    _ => return Err(not_const(inst_span)),
                };

                // Get the struct definition for field name lookup.
                let struct_def = self.type_pool.struct_def(struct_id);
                let field_name_to_idx: HashMap<String, usize> = struct_def
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, f)| (f.name.clone(), i))
                    .collect();

                // Bind each field into locals.
                let destr_fields = self.rir.get_destructure_fields(fields_start, fields_len);
                for field in &destr_fields {
                    if field.is_wildcard {
                        continue;
                    }
                    let field_name = self.interner.resolve(&field.field_name).to_string();
                    let field_idx = match field_name_to_idx.get(&field_name) {
                        Some(&idx) => idx,
                        None => return Err(not_const(inst_span)),
                    };
                    let binding_name = field.binding_name.unwrap_or(field.field_name);
                    locals.insert(binding_name, field_values[field_idx]);
                }
                Ok(ConstValue::Unit)
            }

            // ── Method call ─────────────────────────────────────────────────────
            InstData::MethodCall {
                receiver,
                method,
                args_start,
                args_len,
            } => {
                const COMPTIME_CALL_DEPTH_LIMIT: u32 = 64;

                // Evaluate the receiver to get the value.
                let receiver_val =
                    self.evaluate_comptime_inst(receiver, locals, ctx, outer_span)?;

                // Handle comptime_str method dispatch.
                if let ConstValue::ComptimeStr(str_idx) = receiver_val {
                    let method_name = self.interner.resolve(&method).to_string();
                    let call_args = self.rir.get_call_args(args_start, args_len);
                    return self.evaluate_comptime_str_method(
                        str_idx,
                        &method_name,
                        &call_args,
                        locals,
                        ctx,
                        ComptimeSpans {
                            outer: outer_span,
                            inst: inst_span,
                        },
                    );
                }

                // Determine the struct type from the receiver value.
                let struct_id = match receiver_val {
                    ConstValue::Struct(heap_idx) => match &self.comptime_heap[heap_idx as usize] {
                        ComptimeHeapItem::Struct { struct_id, .. } => *struct_id,
                        _ => return Err(not_const(inst_span)),
                    },
                    _ => return Err(not_const(inst_span)),
                };

                // Look up the method.
                let method_key = (struct_id, method);
                let method_info = match self.methods.get(&method_key) {
                    Some(info) => *info,
                    None => return Err(not_const(inst_span)),
                };

                // Evaluate all explicit arguments.
                let call_args = self.rir.get_call_args(args_start, args_len);
                let mut arg_values = Vec::with_capacity(call_args.len());
                for call_arg in &call_args {
                    let val =
                        self.evaluate_comptime_inst(call_arg.value, locals, ctx, outer_span)?;
                    arg_values.push(val);
                }

                // Enforce call stack depth limit.
                if self.comptime_call_depth >= COMPTIME_CALL_DEPTH_LIMIT {
                    return Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "comptime call stack depth exceeded {} levels",
                                COMPTIME_CALL_DEPTH_LIMIT
                            ),
                        },
                        inst_span,
                    ));
                }

                // Bind self and parameters.
                let param_names = self.param_arena.names(method_info.params).to_vec();
                let mut call_locals: HashMap<Spur, ConstValue> = {
                    let mut m = HashMap::default();
                    m.reserve(param_names.len() + 1);
                    m
                };
                // Bind `self`.
                let self_sym = self.interner.get_or_intern("self");
                call_locals.insert(self_sym, receiver_val);
                for (param_name, arg_val) in param_names.iter().zip(arg_values.iter()) {
                    call_locals.insert(*param_name, *arg_val);
                }

                // Execute the method body.
                self.comptime_call_depth += 1;
                let body_result = self.evaluate_comptime_inst(
                    method_info.body,
                    &mut call_locals,
                    ctx,
                    outer_span,
                );
                self.comptime_call_depth -= 1;
                let body_result = body_result?;

                match body_result {
                    ConstValue::ReturnSignal => {
                        self.comptime_return_value.take().ok_or_else(|| {
                            CompileError::new(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: "comptime return signal missing its value".into(),
                                },
                                inst_span,
                            )
                        })
                    }
                    ConstValue::BreakSignal | ConstValue::ContinueSignal => Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: "break/continue outside a loop in comptime method".into(),
                        },
                        inst_span,
                    )),
                    val => Ok(val),
                }
            }

            // ── Intrinsic ────────────────────────────────────────────────────────
            InstData::Intrinsic {
                name,
                args_start,
                args_len,
            } => {
                // @cast is a no-op in comptime since all integers are i64.
                if name == self.known.cast {
                    let arg_refs = self.rir.get_inst_refs(args_start, args_len);
                    if arg_refs.len() != 1 {
                        return Err(not_const(inst_span));
                    }
                    return self.evaluate_comptime_inst(arg_refs[0], locals, ctx, outer_span);
                }
                // @dbg formats values, prints to stderr on-the-fly (unless
                // suppressed), appends to the comptime dbg buffer, and queues a
                // warning to be emitted after sema completes.
                if name == self.known.dbg {
                    let arg_refs = self.rir.get_inst_refs(args_start, args_len);
                    let mut parts = Vec::with_capacity(arg_refs.len());
                    for &arg_ref in &arg_refs {
                        let val = self.evaluate_comptime_inst(arg_ref, locals, ctx, outer_span)?;
                        parts.push(self.format_const_value(val, inst_span)?);
                    }
                    let msg = parts.join(" ");
                    if !self.suppress_comptime_dbg_print {
                        eprintln!("comptime dbg: {msg}");
                    }
                    self.comptime_dbg_output.push(msg.clone());
                    self.comptime_log_output.push((msg, inst_span));
                    return Ok(ConstValue::Unit);
                }
                // @compile_error emits a user-defined compile error.
                if name == self.known.compile_error {
                    let arg_refs = self.rir.get_inst_refs(args_start, args_len);
                    if arg_refs.len() != 1 {
                        return Err(CompileError::new(
                            ErrorKind::IntrinsicWrongArgCount {
                                name: "compile_error".to_string(),
                                expected: 1,
                                found: arg_refs.len(),
                            },
                            inst_span,
                        ));
                    }
                    let msg =
                        self.evaluate_comptime_string_arg(arg_refs[0], locals, ctx, outer_span)?;
                    return Err(CompileError::new(
                        ErrorKind::ComptimeUserError(msg),
                        inst_span,
                    ));
                }
                // @range produces a comptime array of integers.
                if name == self.known.range {
                    let arg_refs = self.rir.get_inst_refs(args_start, args_len);
                    let (start, end, stride) = match arg_refs.len() {
                        1 => {
                            let end = int(
                                self.evaluate_comptime_inst(arg_refs[0], locals, ctx, outer_span)?,
                                inst_span,
                            )?;
                            (0i64, end, 1i64)
                        }
                        2 => {
                            let s = int(
                                self.evaluate_comptime_inst(arg_refs[0], locals, ctx, outer_span)?,
                                inst_span,
                            )?;
                            let e = int(
                                self.evaluate_comptime_inst(arg_refs[1], locals, ctx, outer_span)?,
                                inst_span,
                            )?;
                            (s, e, 1i64)
                        }
                        3 => {
                            let s = int(
                                self.evaluate_comptime_inst(arg_refs[0], locals, ctx, outer_span)?,
                                inst_span,
                            )?;
                            let e = int(
                                self.evaluate_comptime_inst(arg_refs[1], locals, ctx, outer_span)?,
                                inst_span,
                            )?;
                            let st = int(
                                self.evaluate_comptime_inst(arg_refs[2], locals, ctx, outer_span)?,
                                inst_span,
                            )?;
                            (s, e, st)
                        }
                        _ => {
                            return Err(CompileError::new(
                                ErrorKind::IntrinsicWrongArgCount {
                                    name: "range".to_string(),
                                    expected: 1,
                                    found: arg_refs.len(),
                                },
                                inst_span,
                            ));
                        }
                    };
                    if stride == 0 {
                        return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: "@range stride must not be zero".into(),
                            },
                            inst_span,
                        ));
                    }
                    // Cap the element count to prevent OOM from e.g. @range(i64::MAX)
                    const MAX_RANGE_ELEMENTS: usize = 1_000_000;
                    let mut elements = Vec::new();
                    let mut i = start;
                    if stride > 0 {
                        while i < end {
                            if elements.len() >= MAX_RANGE_ELEMENTS {
                                return Err(CompileError::new(
                                    ErrorKind::ComptimeEvaluationFailed {
                                        reason: format!(
                                            "@range produces too many elements (limit is {})",
                                            MAX_RANGE_ELEMENTS
                                        ),
                                    },
                                    inst_span,
                                ));
                            }
                            elements.push(ConstValue::Integer(i));
                            i = i.checked_add(stride).ok_or_else(|| overflow(inst_span))?;
                        }
                    } else {
                        while i > end {
                            if elements.len() >= MAX_RANGE_ELEMENTS {
                                return Err(CompileError::new(
                                    ErrorKind::ComptimeEvaluationFailed {
                                        reason: format!(
                                            "@range produces too many elements (limit is {})",
                                            MAX_RANGE_ELEMENTS
                                        ),
                                    },
                                    inst_span,
                                ));
                            }
                            elements.push(ConstValue::Integer(i));
                            i = i.checked_add(stride).ok_or_else(|| overflow(inst_span))?;
                        }
                    }
                    let idx = self.comptime_heap.len() as u32;
                    self.comptime_heap.push(ComptimeHeapItem::Array(elements));
                    return Ok(ConstValue::Array(idx));
                }
                // Platform intrinsics return variants of the built-in Os/Arch enums.
                // They are pure functions of the compile target, so the comptime
                // interpreter can evaluate them directly.
                if let Some(id) = self.known.intrinsic_id(name) {
                    match id {
                        IntrinsicId::TargetOs => {
                            let enum_id = self
                                .builtin_os_id
                                .expect("Os enum not injected - internal compiler error");
                            let variant_idx = os_variant_index(self.target.os());
                            return Ok(ConstValue::EnumVariant {
                                enum_id,
                                variant_idx,
                            });
                        }
                        IntrinsicId::TargetArch => {
                            let enum_id = self
                                .builtin_arch_id
                                .expect("Arch enum not injected - internal compiler error");
                            let variant_idx = arch_variant_index(self.target.arch());
                            return Ok(ConstValue::EnumVariant {
                                enum_id,
                                variant_idx,
                            });
                        }
                        _ => {}
                    }
                }
                // Unrecognized intrinsic: surface the name in the diagnostic
                // rather than the generic "cannot be known at compile time"
                // message. In particular, `@compileLog` was removed in favor of
                // `@dbg` — reporting the name guides users to the replacement.
                let intrinsic_name = self.interner.resolve(&name).to_string();
                Err(CompileError::new(
                    ErrorKind::UnknownIntrinsic(intrinsic_name),
                    inst_span,
                ))
            }

            // ── Type intrinsic (@size_of, @align_of, @type_name, @type_info) ──────
            InstData::TypeIntrinsic { name, type_arg } => {
                // Resolve the type argument.
                // Check comptime_type_overrides first (for generic type params like T),
                // then comptime_type_vars from the analysis context, then fall back
                // to the normal type resolver.
                let ty = if let Some(&override_ty) = self.comptime_type_overrides.get(&type_arg) {
                    override_ty
                } else if let Some(&ctx_ty) = ctx.comptime_type_vars.get(&type_arg) {
                    ctx_ty
                } else if let Some(&var_ty) = locals.iter().find_map(|(k, v)| {
                    if *k == type_arg {
                        if let ConstValue::Type(t) = v {
                            Some(t)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }) {
                    var_ty
                } else {
                    self.resolve_type(type_arg, inst_span)
                        .map_err(|_| not_const(inst_span))?
                };
                match self.known.intrinsic_id(name) {
                    Some(IntrinsicId::SizeOf) => {
                        let slot_count = self.abi_slot_count(ty);
                        Ok(ConstValue::Integer((slot_count as i64) * 8))
                    }
                    Some(IntrinsicId::AlignOf) => {
                        let slot_count = self.abi_slot_count(ty);
                        Ok(ConstValue::Integer(if slot_count == 0 { 1 } else { 8 }))
                    }
                    Some(IntrinsicId::TypeName) => self.evaluate_comptime_type_name(ty, inst_span),
                    Some(IntrinsicId::TypeInfo) => self.evaluate_comptime_type_info(ty, inst_span),
                    Some(IntrinsicId::Ownership) => {
                        let enum_id = self
                            .builtin_ownership_id
                            .expect("Ownership enum not injected - internal compiler error");
                        let variant_idx = self.ownership_variant_index(ty);
                        Ok(ConstValue::EnumVariant {
                            enum_id,
                            variant_idx,
                        })
                    }
                    _ => Err(not_const(inst_span)),
                }
            }

            // ── Type+interface intrinsic (@implements) ────────────────────────
            InstData::TypeInterfaceIntrinsic {
                name,
                type_arg,
                type_inst,
                interface_arg,
            } => {
                let ty = if let Some(t_inst) = type_inst {
                    // ADR-0079: arg[0] is an arbitrary expression
                    // (e.g. `f.field_type`); evaluate it recursively
                    // inside the same comptime evaluation so the
                    // outer heap stays intact.
                    match self.evaluate_comptime_inst(t_inst, locals, ctx, outer_span)? {
                        ConstValue::Type(t) => t,
                        _ => return Err(not_const(inst_span)),
                    }
                } else if let Some(&override_ty) = self.comptime_type_overrides.get(&type_arg) {
                    override_ty
                } else if let Some(&ctx_ty) = ctx.comptime_type_vars.get(&type_arg) {
                    ctx_ty
                } else if let Some(&val) = locals.get(&type_arg)
                    && let ConstValue::Type(t) = val
                {
                    t
                } else {
                    self.resolve_type(type_arg, inst_span)
                        .map_err(|_| not_const(inst_span))?
                };
                match self.known.intrinsic_id(name) {
                    Some(IntrinsicId::Implements) => {
                        let interface_id = self
                            .interfaces
                            .get(&interface_arg)
                            .copied()
                            .ok_or_else(|| not_const(inst_span))?;
                        let value = self.check_conforms(ty, interface_id, inst_span).is_ok();
                        Ok(ConstValue::Bool(value))
                    }
                    _ => Err(not_const(inst_span)),
                }
            }

            // ── Not yet supported ─────────────────────────────────────────────
            _ => Err(not_const(inst_span)),
        }
    }

    /// Check if an RIR instruction is a VarRef to a comptime type variable.
    ///
    /// This is used when validating comptime arguments to detect variables
    /// that hold comptime type values (e.g., `let P = Point(); ... Line(P)`).
    pub(crate) fn is_comptime_type_var(&self, inst_ref: InstRef, ctx: &AnalysisContext) -> bool {
        if let InstData::VarRef { name } = &self.rir.get(inst_ref).data {
            ctx.comptime_type_vars.contains_key(name)
        } else {
            false
        }
    }

    /// Resolve an enum type by name during comptime evaluation.
    /// Checks direct enums first, then comptime type variables.
    fn resolve_comptime_enum(&self, type_name: Spur, ctx: &AnalysisContext) -> Option<EnumId> {
        if let Some(&id) = self.enums.get(&type_name) {
            Some(id)
        } else if let Some(&ty) = self.comptime_type_overrides.get(&type_name) {
            match ty.kind() {
                TypeKind::Enum(id) => Some(id),
                _ => None,
            }
        } else if let Some(&ty) = ctx.comptime_type_vars.get(&type_name) {
            match ty.kind() {
                TypeKind::Enum(id) => Some(id),
                _ => None,
            }
        } else {
            None
        }
    }

    /// Resolve a struct type by name during comptime evaluation.
    /// Checks direct structs first, then comptime type overrides, then comptime type variables.
    fn resolve_comptime_struct(&self, type_name: Spur, ctx: &AnalysisContext) -> Option<StructId> {
        if let Some(&id) = self.structs.get(&type_name) {
            Some(id)
        } else if let Some(&ty) = self.comptime_type_overrides.get(&type_name) {
            match ty.kind() {
                TypeKind::Struct(id) => Some(id),
                _ => None,
            }
        } else if let Some(&ty) = ctx.comptime_type_vars.get(&type_name) {
            match ty.kind() {
                TypeKind::Struct(id) => Some(id),
                _ => None,
            }
        } else {
            None
        }
    }
}

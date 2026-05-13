//! Instruction category analysis methods.
//!
//! This module contains the per-category analysis methods extracted from `analyze_inst`.
//! Each category method handles a specific group of related RIR instructions:
//!
//! - [`analyze_literal`] - Integer, boolean, string, and unit constants
//! - [`analyze_unary_op`] - Negation, logical NOT, bitwise NOT
//! - [`analyze_control_flow`] - Branch, Loop, InfiniteLoop, Match, Break, Continue, Ret, Block
//! - [`analyze_variable_ops`] - Alloc, VarRef, ParamRef, Assign
//! - [`analyze_struct_ops`] - StructDecl, StructInit, FieldGet, FieldSet
//! - [`analyze_array_ops`] - ArrayInit, IndexGet, IndexSet
//! - [`analyze_enum_ops`] - EnumDecl, EnumVariant
//! - [`analyze_call_ops`] - Call, MethodCall, AssocFnCall
//! - [`analyze_intrinsic_ops`] - Intrinsic, TypeIntrinsic
//! - [`analyze_decl_noop`] - declarations that produce Unit
//!
//! Binary operations (arithmetic, comparison, logical, bitwise) are handled
//! by existing helper methods in `analysis.rs`:
//! - `analyze_binary_arith` - Add, Sub, Mul, Div, Mod, BitAnd, BitOr, BitXor, Shl, Shr
//! - `analyze_comparison` - Eq, Ne, Lt, Gt, Le, Ge
//! - Logical And/Or are simple enough to remain inline

use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

use gruel_builtins::Posture;
use gruel_intrinsics::IntrinsicId;
use gruel_rir::{
    InstData, InstRef, RirArgMode, RirCallArg, RirDestructureField, RirParamMode, RirPattern,
    RirPatternBinding,
};
use gruel_util::{
    CompileError, CompileResult, CompileWarning, ErrorKind, MissingFieldsError, OptionExt,
    WarningKind,
};
use lasso::Spur;

use crate::sema::context::ConstValue;
use gruel_util::{BinOp, Span, UnaryOp};

use super::Sema;
use super::context::{AnalysisContext, AnalysisResult, LocalVar};
use crate::inst::{
    Air, AirArgMode, AirCallArg, AirInst, AirInstData, AirPattern, AirPlaceBase, AirPlaceRef,
    AirProjection, AirRef,
};
use crate::scope::ScopedContext;
use crate::types::{StructField, Type, TypeKind};

/// Source of a receiver for ADR-0055 call-sugar. The call-sugar path needs
/// to emit a load from either a local slot or a parameter ABI slot, so we
/// distinguish them here.
enum ReceiverSource {
    Local(u32),
    Param(u32),
}

/// Header info shared by every `for` loop analyzer (range / array / slice).
///
/// The four fields here are decided by the parser before the analyzer can
/// know which iterable shape it's dealing with, so they pass through every
/// downstream variant.
struct ForLoopHead {
    binding: Spur,
    is_mut: bool,
    body: InstRef,
    span: Span,
}

/// Per-iteration metadata for an array `for` loop.
struct ArraySource {
    air_ref: AirRef,
    ty: Type,
    elem_ty: Type,
    len: u64,
    is_copy: bool,
}

/// Output sink for pattern binding emission. Both vectors are appended to
/// in order as the recursive pattern walker introduces local slots.
pub(crate) struct BindingEmission<'a> {
    pub(crate) storage_lives: &'a mut Vec<AirRef>,
    pub(crate) allocs: &'a mut Vec<AirRef>,
}

/// Resolved receiver for an ADR-0055 call-sugar dispatch.
struct CallSugarReceiver {
    name: Spur,
    ty: Type,
    source: ReceiverSource,
    struct_id: crate::types::StructId,
}

/// Reference to an enum variant in source code: an optional module qualifier
/// (e.g. `crate::Mod::Enum::Variant`), the enum's type name, and the
/// variant's name.
struct VariantRef {
    module: Option<InstRef>,
    type_name: Spur,
    variant: Spur,
}

/// Field value extracted from a destructuring pattern: the AIR value of the
/// field, its type, and the source span of the binding for diagnostics.
struct BoundField {
    val: AirRef,
    ty: Type,
    span: Span,
}

// ============================================================================
// Place Building (ADR-0030 Phase 8)
// ============================================================================

/// Projection info collected during place tracing.
///
/// This extends `AirProjection` with additional metadata needed for type checking
/// and move analysis.
#[derive(Debug)]
pub(crate) struct ProjectionInfo {
    /// The projection to emit
    pub proj: AirProjection,
    /// The type resulting from this projection
    pub result_type: Type,
    /// For field projections: the field name (for error messages)
    /// For index projections: None
    #[allow(dead_code)]
    pub field_name: Option<Spur>,
}

/// Result of tracing a place expression in RIR.
///
/// This contains all the information needed to build an `AirPlace` and emit
/// a `PlaceRead` or `PlaceWrite` instruction.
#[derive(Debug)]
pub(crate) struct PlaceTrace {
    /// The base of the place (local slot or param slot)
    pub base: AirPlaceBase,
    /// The type of the base (before projections)
    pub base_type: Type,
    /// Projections collected during tracing (in order from base to leaf)
    pub projections: Vec<ProjectionInfo>,
    /// The root variable name (for move checking)
    pub root_var: Spur,
    /// Whether the root is mutable (for write validation)
    pub is_root_mutable: bool,
    /// Whether this is a borrow parameter (for error messages)
    pub is_borrow_param: bool,
}

/// ADR-0076 collapse: when a place's result type is `Ref(T)` /
/// `MutRef(T)`, projecting into the referent should look up fields /
/// indices on `T`. The binding's storage IS the pointer (params are
/// by-pointer at the LLVM ABI level for ref types), so the GEP starts
/// at the same base pointer — no extra dereference is added at codegen.
pub(crate) fn unwrap_ref_for_place<'a>(sema: &super::Sema<'a>, ty: Type) -> Type {
    match ty.kind() {
        crate::types::TypeKind::Ref(id) => sema.type_pool.ref_def(id),
        crate::types::TypeKind::MutRef(id) => sema.type_pool.mut_ref_def(id),
        _ => ty,
    }
}

/// ADR-0076 collapse: derive (mutable, is_borrow_param) flags from a
/// parameter's type and mode. A `MutRef(T)` typed parameter is treated as
/// mutable for write-validation purposes (write-through is allowed); a
/// `Ref(T)` typed parameter is treated as a borrow (writes rejected).
fn param_kind_flags(param: &super::context::ParamInfo) -> (bool, bool) {
    match param.ty.kind() {
        crate::types::TypeKind::MutRef(_) => (true, false),
        crate::types::TypeKind::Ref(_) => (false, true),
        _ => match param.mode {
            gruel_rir::RirParamMode::MutRef => (true, false),
            gruel_rir::RirParamMode::Ref => (false, true),
            _ => (false, false),
        },
    }
}

/// ADR-0076 collapse: same for a local binding. A local of type
/// `MutRef(T)` is mutable through-write regardless of the binding's
/// `let mut` annotation (rebinding a ref is meaningless; write-through
/// is the only useful operation).
fn local_kind_flags(local: &LocalVar) -> (bool, bool) {
    match local.ty.kind() {
        crate::types::TypeKind::MutRef(_) => (true, false),
        crate::types::TypeKind::Ref(_) => (false, true),
        _ => (local.is_mut, false),
    }
}

impl PlaceTrace {
    /// Get the final type of the place (after all projections).
    pub fn result_type(&self) -> Type {
        self.projections
            .last()
            .map(|p| p.result_type)
            .unwrap_or(self.base_type)
    }
}

/// Convert a `RirPatternBinding` into its recursive `AirPattern` form for
/// ADR-0051 `lower_pattern`. Wildcard bindings → `Wildcard`; simple named
/// bindings → `Bind` leaves; bindings carrying a nested `sub_pattern` →
/// whatever `sub_pattern` lowers to (recursively).
fn binding_to_pattern(sema: &Sema<'_>, b: &RirPatternBinding) -> AirPattern {
    if let Some(sub) = &b.sub_pattern {
        // Nested sub-patterns go through the full recursive lowering. The
        // `resolved_enum` for the sub-pattern is looked up fresh when
        // `lower_pattern` recurses into a DataVariant / StructVariant.
        return sema.lower_pattern(sub, sema.resolve_enum_from_pattern(sub));
    }
    if b.is_wildcard {
        AirPattern::Wildcard
    } else if let Some(name) = b.name {
        AirPattern::Bind {
            name,
            is_mut: b.is_mut,
            inner: None,
        }
    } else {
        // Shouldn't reach here — bindings with neither name nor sub_pattern
        // and that aren't wildcards are malformed. Fall back to wildcard.
        AirPattern::Wildcard
    }
}

/// ADR-0051 Phase 4c: expand a tuple pattern's `..` rest marker into
/// wildcards filling the scrutinee's arity. Called from sema before
/// `lower_pattern` and `emit_recursive_pattern_bindings` so downstream
/// code sees a flat element list with `rest_position = None`. For
/// non-tuple patterns or tuples without rest, returns the input
/// unchanged.
fn expanded_tuple_pattern(
    pattern: &RirPattern,
    type_pool: &crate::intern_pool::TypeInternPool,
    scr_ty: Type,
) -> RirPattern {
    let RirPattern::Tuple {
        elems,
        rest_position: Some(pos),
        span,
    } = pattern
    else {
        return pattern.clone();
    };
    let Some(struct_id) = scr_ty.as_struct() else {
        return pattern.clone();
    };
    let arity = type_pool.struct_def(struct_id).fields.len();
    let pos = *pos as usize;
    let prefix_len = pos.min(elems.len());
    let suffix_len = elems.len() - prefix_len;
    let wildcards = arity.saturating_sub(prefix_len + suffix_len);
    let mut new_elems = Vec::with_capacity(arity);
    new_elems.extend(elems[..prefix_len].iter().cloned());
    for _ in 0..wildcards {
        new_elems.push(RirPattern::Wildcard(*span));
    }
    new_elems.extend(elems[prefix_len..].iter().cloned());
    RirPattern::Tuple {
        elems: new_elems,
        rest_position: None,
        span: *span,
    }
}

/// ADR-0079 Phase 3: resolve a logical field name on an enum variant.
/// Struct variants store names directly; tuple variants address fields
/// positionally as `"0"`, `"1"`, …; unit variants have no fields.
fn variant_field_index(variant: &crate::types::EnumVariantDef, name: &str) -> Option<usize> {
    if variant.is_struct_variant() {
        variant.field_names.iter().position(|n| n == name)
    } else {
        // Tuple/unit variant: positional. Match digit strings.
        name.parse::<usize>()
            .ok()
            .filter(|&i| i < variant.fields.len())
    }
}

/// Render the canonical field name for the i'th position of a variant.
fn variant_field_name(variant: &crate::types::EnumVariantDef, i: usize) -> String {
    if variant.is_struct_variant() {
        variant.field_names[i].clone()
    } else {
        format!("{}", i)
    }
}

impl<'a> Sema<'a> {
    // ========================================================================
    // Place Tracing (ADR-0030 Phase 8)
    // ========================================================================

    /// Try to trace an RIR expression to a place (lvalue).
    ///
    /// This walks the RIR instruction chain backward from a `FieldGet` or `IndexGet`
    /// to find the root `VarRef` or `ParamRef`, collecting projections along the way.
    ///
    /// Returns `None` if the expression is not a place (e.g., a function call result).
    ///
    /// # Arguments
    /// * `inst_ref` - The RIR instruction to trace
    /// * `air` - The AIR being built (needed to analyze index expressions)
    /// * `ctx` - Analysis context with local/param info
    ///
    /// # Returns
    /// * `Some(PlaceTrace)` if the expression is a place
    /// * `None` if it's not (e.g., `get_struct().field` where base is a call)
    pub(crate) fn try_trace_place(
        &mut self,
        inst_ref: InstRef,
        air: &mut Air,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Option<PlaceTrace>> {
        self.try_trace_place_inner(inst_ref, air, ctx)
    }

    /// Inner implementation that accumulates projections.
    fn try_trace_place_inner(
        &mut self,
        inst_ref: InstRef,
        air: &mut Air,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Option<PlaceTrace>> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            // Base case: local variable reference
            InstData::VarRef { name } => {
                // First check if it's actually a parameter
                if let Some(param_info) = ctx.params.iter().find(|p| p.name == *name) {
                    let (mutable, is_borrow) = param_kind_flags(param_info);
                    // ADR-0076: a `Ref(T)` / `MutRef(T)` binding's storage
                    // is the pointer; its place semantics target the
                    // referent `T`. Unwrapping here lets every downstream
                    // projection (field, index) operate on `T` without
                    // each site repeating the auto-deref.
                    let base_type = unwrap_ref_for_place(self, param_info.ty);
                    return Ok(Some(PlaceTrace {
                        base: AirPlaceBase::Param(param_info.abi_slot),
                        base_type,
                        projections: Vec::new(),
                        root_var: *name,
                        is_root_mutable: mutable,
                        is_borrow_param: is_borrow,
                    }));
                }

                // Check if it's a local variable
                if let Some(local) = ctx.locals.get(name) {
                    let (mutable, is_borrow) = local_kind_flags(local);
                    let base_type = unwrap_ref_for_place(self, local.ty);
                    return Ok(Some(PlaceTrace {
                        base: AirPlaceBase::Local(local.slot),
                        base_type,
                        projections: Vec::new(),
                        root_var: *name,
                        is_root_mutable: mutable,
                        is_borrow_param: is_borrow,
                    }));
                }

                // Not a variable - might be a constant or type name
                Ok(None)
            }

            // Base case: explicit parameter reference
            InstData::ParamRef { name, .. } => {
                if let Some(param_info) = ctx.params.iter().find(|p| p.name == *name) {
                    let (mutable, is_borrow) = param_kind_flags(param_info);
                    let base_type = unwrap_ref_for_place(self, param_info.ty);
                    return Ok(Some(PlaceTrace {
                        base: AirPlaceBase::Param(param_info.abi_slot),
                        base_type,
                        projections: Vec::new(),
                        root_var: *name,
                        is_root_mutable: mutable,
                        is_borrow_param: is_borrow,
                    }));
                }
                Ok(None)
            }

            // Recursive case: field access
            InstData::FieldGet { base, field } => {
                // First, recursively trace the base
                let base_trace = self.try_trace_place_inner(*base, air, ctx)?;

                match base_trace {
                    Some(mut trace) => {
                        // ADR-0076: auto-deref through `Ref(T)` / `MutRef(T)`.
                        let base_type = unwrap_ref_for_place(self, trace.result_type());
                        let struct_id = match base_type.as_struct() {
                            Some(id) => id,
                            None => {
                                // Module access or non-struct - not a place
                                return Ok(None);
                            }
                        };

                        // Look up field info. Phase 6 emits `..end_N`
                        // markers for suffix positions in tuple-root
                        // match arms (`(a, .., b)`); resolve those
                        // against the tuple's arity before normal
                        // lookup.
                        let struct_def = self.type_pool.struct_def(struct_id);
                        let field_name_str = self.interner.resolve(field).to_string();
                        let (resolved_field, resolved_name_str) =
                            if let Some(rest) = field_name_str.strip_prefix("..end_") {
                                let from_end: usize = match rest.parse() {
                                    Ok(n) => n,
                                    Err(_) => return Ok(None),
                                };
                                let arity = struct_def.fields.len();
                                if from_end >= arity {
                                    return Ok(None);
                                }
                                let idx = arity - 1 - from_end;
                                let idx_str = idx.to_string();
                                let new_spur = self.interner.get_or_intern(&idx_str);
                                (new_spur, idx_str)
                            } else {
                                (*field, field_name_str)
                            };
                        let (field_index, struct_field) =
                            match struct_def.find_field(&resolved_name_str) {
                                Some(info) => info,
                                None => return Ok(None), // Unknown field
                            };

                        // ADR-0073: unified visibility check (subsumes the
                        // ADR-0072 builtin-private path; built-ins always
                        // run, user-defined types only under preview).
                        self.check_field_visibility(
                            &struct_def,
                            struct_field,
                            self.rir.get(inst_ref).span,
                        )?;

                        let field_type = struct_field.ty;

                        // Add this projection with field name for move checking
                        trace.projections.push(ProjectionInfo {
                            proj: AirProjection::Field {
                                struct_id,
                                field_index: field_index as u32,
                            },
                            result_type: field_type,
                            field_name: Some(resolved_field),
                        });

                        Ok(Some(trace))
                    }
                    None => {
                        // Base is not a place (e.g., function call result)
                        Ok(None)
                    }
                }
            }

            // Recursive case: array index
            InstData::IndexGet { base, index } => {
                // First, recursively trace the base
                let base_trace = self.try_trace_place_inner(*base, air, ctx)?;

                match base_trace {
                    Some(mut trace) => {
                        // ADR-0076: auto-deref through `Ref(T)` / `MutRef(T)`.
                        // The binding's storage IS the pointer (params are
                        // by-pointer at the LLVM ABI level for ref types),
                        // so projecting into the referent doesn't add an
                        // extra dereference at codegen — the GEP starts at
                        // the same base pointer.
                        let base_type = unwrap_ref_for_place(self, trace.result_type());
                        let (_array_type_id, elem_type) = match base_type.as_array() {
                            Some(id) => {
                                let (elem, _len) = self.type_pool.array_def(id);
                                (id, elem)
                            }
                            None => return Ok(None), // Not an array
                        };

                        // Analyze the index expression to get an AirRef
                        let index_result = self.analyze_inst(air, *index, ctx)?;

                        // Add this projection (no field name for indices)
                        trace.projections.push(ProjectionInfo {
                            proj: AirProjection::Index {
                                array_type: base_type,
                                index: index_result.air_ref,
                            },
                            result_type: elem_type,
                            field_name: None,
                        });

                        Ok(Some(trace))
                    }
                    None => {
                        // Base is not a place
                        Ok(None)
                    }
                }
            }

            // Not a place expression
            _ => Ok(None),
        }
    }

    /// Build an AirPlaceRef from a PlaceTrace, adding projections to the Air.
    pub(crate) fn build_place_ref(air: &mut Air, trace: &PlaceTrace) -> AirPlaceRef {
        let projs = trace.projections.iter().map(|p| p.proj);
        air.make_place(trace.base, projs)
    }

    // ========================================================================
    // Literals: IntConst, BoolConst, StringConst, UnitConst
    // ========================================================================

    /// Analyze a literal constant instruction.
    ///
    /// Handles: IntConst, BoolConst, StringConst, UnitConst
    pub(crate) fn analyze_literal(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::IntConst(value) => {
                // Get the type from HM inference
                let ty = Self::get_resolved_type(ctx, inst_ref, inst.span, "integer literal")?;

                // Check if the literal value fits in the target type's range
                if !ty.literal_fits(*value) {
                    return Err(CompileError::new(
                        ErrorKind::LiteralOutOfRange {
                            value: *value,
                            ty: ty.name().to_string(),
                        },
                        inst.span,
                    ));
                }

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Const(*value),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::FloatConst(bits) => {
                let ty =
                    Self::get_resolved_type(ctx, inst_ref, inst.span, "floating-point literal")?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::FloatConst(*bits),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::BoolConst(value) => {
                let ty = Type::BOOL;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::BoolConst(*value),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            // ADR-0071: char literal — lowers to a 32-bit integer constant
            // holding the Unicode scalar value.
            InstData::CharConst(value) => {
                let ty = Type::CHAR;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Const(*value as u64),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::StringConst(symbol) => {
                // String literals use the builtin String struct type.
                let ty = self.builtin_string_type();
                // Add string to the local string table (per-function for parallel analysis)
                let string_content = self.interner.resolve(symbol).to_string();
                let local_string_id = ctx.add_local_string(string_content);

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::StringConst(local_string_id),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::UnitConst => {
                let ty = Type::UNIT;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_literal called with non-literal instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }

    // ========================================================================
    // Unary operations: Neg, Not, BitNot
    // ========================================================================

    /// Analyze a unary operator instruction.
    ///
    /// Handles: Neg, Not, BitNot
    pub(crate) fn analyze_unary_op(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        let (op, operand) = match &inst.data {
            InstData::Unary { op, operand } => (*op, *operand),
            _ => {
                return Err(CompileError::new(
                    ErrorKind::InternalError(format!(
                        "analyze_unary_op called with non-unary instruction: {:?}",
                        inst.data
                    )),
                    inst.span,
                ));
            }
        };

        match op {
            UnaryOp::Neg => {
                let ty = Self::get_resolved_type(ctx, inst_ref, inst.span, "negation operator")?;

                if ty.is_unsigned() {
                    return Err(CompileError::new(
                        ErrorKind::CannotNegateUnsigned(ty.name().to_string()),
                        inst.span,
                    )
                    .with_note("unsigned values cannot be negated"));
                }

                // Special case: negating a literal that equals |MIN| for signed types.
                let operand_inst = self.rir.get(operand);
                if let InstData::IntConst(value) = &operand_inst.data
                    && ty.negated_literal_fits(*value)
                    && !ty.literal_fits(*value)
                {
                    let neg_value = match ty.kind() {
                        TypeKind::I8 => (i8::MIN as i64) as u64,
                        TypeKind::I16 => (i16::MIN as i64) as u64,
                        TypeKind::I32 => (i32::MIN as i64) as u64,
                        TypeKind::I64 => i64::MIN as u64,
                        _ => unreachable!(),
                    };
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Const(neg_value),
                        ty,
                        span: inst.span,
                    });
                    return Ok(AnalysisResult::new(air_ref, ty));
                }

                let operand_result = self.analyze_inst(air, operand, ctx)?;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Unary(UnaryOp::Neg, operand_result.air_ref),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            UnaryOp::Not => {
                let operand_result = self.analyze_inst(air, operand, ctx)?;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Unary(UnaryOp::Not, operand_result.air_ref),
                    ty: Type::BOOL,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::BOOL))
            }

            UnaryOp::BitNot => {
                let ty = Self::get_resolved_type(ctx, inst_ref, inst.span, "bitwise NOT operator")?;

                if !ty.is_integer() && !ty.is_error() && !ty.is_never() {
                    return Err(CompileError::type_mismatch(
                        "integer type".to_string(),
                        ty.name().to_string(),
                        inst.span,
                    ));
                }

                let operand_result = self.analyze_inst(air, operand, ctx)?;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Unary(UnaryOp::BitNot, operand_result.air_ref),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }
        }
    }

    // ========================================================================
    // Reference construction (ADR-0062): &x / &mut x
    // ========================================================================

    /// Analyze a `&x` or `&mut x` reference-construction expression (ADR-0062).
    ///
    /// - Operand must be an lvalue (variable, parameter, or place expression).
    /// - Result type is `Ref(T)` for `&x`, `MutRef(T)` for `&mut x`, where
    ///   `T` is the operand's type.
    pub(crate) fn analyze_make_ref(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);
        let (operand, is_mut) = match &inst.data {
            InstData::MakeRef { operand, is_mut } => (*operand, *is_mut),
            _ => unreachable!("analyze_make_ref called with non-MakeRef instruction"),
        };
        let span = inst.span;

        // Operand must be an lvalue — track from the unanalyzed RIR so we
        // catch e.g. `&(1 + 2)` without depending on AIR shape.
        let root_var = self.extract_root_variable(operand).ok_or_else(|| {
            let kind = if is_mut {
                ErrorKind::InoutNonLvalue
            } else {
                ErrorKind::BorrowNonLvalue
            };
            CompileError::new(kind, self.rir.get(operand).span)
        })?;

        // `&mut x` requires the root binding to be mutable, mirroring the
        // existing `inout`/assignment rules.
        if is_mut
            && let Some(local) = ctx.locals.get(&root_var)
            && !local.is_mut
        {
            let name = self.interner.resolve(&root_var).to_string();
            return Err(CompileError::new(ErrorKind::AssignToImmutable(name), span));
        }

        let operand_result = self.analyze_inst(air, operand, ctx)?;
        let operand_ty = operand_result.ty;

        // Construct the Ref(T) / MutRef(T) type via the intern pool.
        let result_ty = if is_mut {
            let id = self.type_pool.intern_mut_ref_from_type(operand_ty);
            Type::new_mut_ref(id)
        } else {
            let id = self.type_pool.intern_ref_from_type(operand_ty);
            Type::new_ref(id)
        };

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::MakeRef {
                operand: operand_result.air_ref,
                is_mut,
            },
            ty: result_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_ty))
    }

    /// Analyze `&arr[range]` / `&mut arr[range]` (ADR-0064).
    ///
    /// The base must designate an lvalue place of array type. `lo` and `hi`
    /// are optional integer expressions; when both are constant the bounds
    /// are checked at compile time. The result type is `Slice(T)` /
    /// `MutSlice(T)` over the array element type.
    pub(crate) fn analyze_make_slice(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);
        let (base, lo_opt, hi_opt, is_mut) = match &inst.data {
            InstData::MakeSlice {
                base,
                lo,
                hi,
                is_mut,
            } => (*base, *lo, *hi, *is_mut),
            _ => unreachable!("analyze_make_slice called with non-MakeSlice instruction"),
        };
        let span = inst.span;

        // The base must be an lvalue (variable, parameter, or place
        // expression — same rule as `&x` / `&mut x`).
        let root_var = self.extract_root_variable(base).ok_or_else(|| {
            let kind = if is_mut {
                ErrorKind::InoutNonLvalue
            } else {
                ErrorKind::BorrowNonLvalue
            };
            CompileError::new(kind, self.rir.get(base).span)
        })?;

        // `&mut arr[..]` requires the root binding to be mutable.
        if is_mut
            && let Some(local) = ctx.locals.get(&root_var)
            && !local.is_mut
        {
            let name = self.interner.resolve(&root_var).to_string();
            return Err(CompileError::new(ErrorKind::AssignToImmutable(name), span));
        }

        let base_result = self.analyze_inst(air, base, ctx)?;
        let base_ty = base_result.ty;

        let (elem_ty, array_len, base_is_vec) = match base_ty.kind() {
            TypeKind::Array(id) => {
                let (elem, len) = self.type_pool.array_def(id);
                (elem, len, false)
            }
            // ADR-0066: range subscripts on Vec(T) produce slices over the
            // live `[0..len]` range. Compile-time bounds checks against a
            // fixed length don't apply (len is runtime); pass 0 as the
            // sentinel bound so they're skipped, and rely on the codegen
            // bounds check when the slice is constructed.
            TypeKind::Vec(id) => {
                let elem = self.type_pool.vec_def(id);
                (elem, 0u64, true)
            }
            _ if base_ty.is_error() || base_ty.is_never() => (Type::ERROR, 0u64, false),
            _ => {
                return Err(CompileError::new(
                    ErrorKind::IndexOnNonArray {
                        found: self.format_type_name(base_ty),
                    },
                    span,
                ));
            }
        };

        let analyze_endpoint = |this: &mut Self,
                                air: &mut Air,
                                ctx: &mut AnalysisContext,
                                e_ref: Option<InstRef>|
         -> CompileResult<Option<AnalysisResult>> {
            match e_ref {
                None => Ok(None),
                Some(r) => {
                    let result = this.analyze_inst(air, r, ctx)?;
                    if !result.ty.is_integer() && !result.ty.is_error() && !result.ty.is_never() {
                        return Err(CompileError::type_mismatch(
                            "usize".to_string(),
                            this.format_type_name(result.ty),
                            this.rir.get(r).span,
                        ));
                    }
                    Ok(Some(result))
                }
            }
        };

        let lo_result = analyze_endpoint(self, air, ctx, lo_opt)?;
        let hi_result = analyze_endpoint(self, air, ctx, hi_opt)?;

        // Compile-time bounds check when both endpoints are integer
        // constants and the array length is statically known.
        let const_lo = lo_opt.and_then(|r| self.const_int_value(r));
        let const_hi = hi_opt.and_then(|r| self.const_int_value(r));
        let lo_default = const_lo.unwrap_or(0);
        let hi_default = const_hi.or(if lo_opt.is_none() && hi_opt.is_none() {
            Some(array_len as i128)
        } else {
            None
        });
        if !base_ty.is_error()
            && !base_is_vec
            && let Some(hi_v) = hi_default
        {
            if lo_default < 0 || hi_v < 0 || lo_default > hi_v {
                return Err(CompileError::new(
                    ErrorKind::IndexOutOfBounds {
                        index: lo_default as i64,
                        length: array_len,
                    },
                    span,
                ));
            }
            if (hi_v as u64) > array_len {
                return Err(CompileError::new(
                    ErrorKind::IndexOutOfBounds {
                        index: hi_v as i64,
                        length: array_len,
                    },
                    span,
                ));
            }
        }

        let result_ty = if base_ty.is_error() {
            Type::ERROR
        } else if is_mut {
            let id = self.type_pool.intern_mut_slice_from_type(elem_ty);
            Type::new_mut_slice(id)
        } else {
            let id = self.type_pool.intern_slice_from_type(elem_ty);
            Type::new_slice(id)
        };

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::MakeSlice {
                base: base_result.air_ref,
                lo: lo_result.as_ref().map(|r| r.air_ref),
                hi: hi_result.as_ref().map(|r| r.air_ref),
                is_mut,
            },
            ty: result_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_ty))
    }

    /// Recover the integer value of a RIR instruction if it's a literal
    /// `IntConst`, with an inverted unary minus on top.
    fn const_int_value(&self, inst_ref: InstRef) -> Option<i128> {
        match &self.rir.get(inst_ref).data {
            InstData::IntConst(v) => Some(*v as i128),
            InstData::Unary {
                op: UnaryOp::Neg,
                operand,
            } => self.const_int_value(*operand).map(|v| -v),
            _ => None,
        }
    }

    // ========================================================================
    // Logical operations: And, Or
    // ========================================================================

    /// Analyze a logical operator instruction.
    ///
    /// Handles: And, Or
    pub(crate) fn analyze_logical_op(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        let (op, lhs, rhs) = match &inst.data {
            InstData::Bin {
                op: op @ (BinOp::And | BinOp::Or),
                lhs,
                rhs,
            } => (*op, *lhs, *rhs),
            _ => {
                return Err(CompileError::new(
                    ErrorKind::InternalError(format!(
                        "analyze_logical_op called with non-logical instruction: {:?}",
                        inst.data
                    )),
                    inst.span,
                ));
            }
        };

        let lhs_result = self.analyze_inst(air, lhs, ctx)?;
        let rhs_result = self.analyze_inst(air, rhs, ctx)?;
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Bin(op, lhs_result.air_ref, rhs_result.air_ref),
            ty: Type::BOOL,
            span: inst.span,
        });
        Ok(AnalysisResult::new(air_ref, Type::BOOL))
    }

    // ========================================================================
    // Control flow: Branch, Loop, InfiniteLoop, Match, Break, Continue, Ret, Block
    // ========================================================================

    /// Analyze a control flow instruction.
    ///
    /// Handles: Branch, Loop, InfiniteLoop, Match, Break, Continue, Ret, Block
    pub(crate) fn analyze_control_flow(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::Branch {
                cond,
                then_block,
                else_block,
                is_comptime,
            } => self.analyze_branch(
                air,
                *cond,
                *then_block,
                *else_block,
                *is_comptime,
                inst.span,
                ctx,
            ),

            InstData::Loop { cond, body } => {
                self.analyze_while_loop(air, *cond, *body, inst.span, ctx)
            }

            InstData::For {
                binding,
                is_mut,
                iterable,
                body,
            } => {
                let head = ForLoopHead {
                    binding: *binding,
                    is_mut: *is_mut,
                    body: *body,
                    span: inst.span,
                };
                self.analyze_for_loop(air, head, *iterable, ctx)
            }

            InstData::InfiniteLoop { body } => {
                self.analyze_infinite_loop(air, *body, inst.span, ctx)
            }

            InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            } => self.analyze_match(air, *scrutinee, *arms_start, *arms_len, inst.span, ctx),

            InstData::Break => {
                // Validate that we're inside a loop
                if ctx.loop_depth == 0 {
                    return Err(CompileError::new(ErrorKind::BreakOutsideLoop, inst.span));
                }

                // Check if break is forbidden (e.g., consuming for-in loop)
                if let Some(ref elem_type_name) = ctx.forbid_break {
                    return Err(CompileError::new(
                        ErrorKind::BreakInConsumingForLoop {
                            element_type: elem_type_name.clone(),
                        },
                        inst.span,
                    ));
                }

                // Break has the never type - it diverges
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Break,
                    ty: Type::NEVER,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::NEVER))
            }

            InstData::Continue => {
                // Validate that we're inside a loop
                if ctx.loop_depth == 0 {
                    return Err(CompileError::new(ErrorKind::ContinueOutsideLoop, inst.span));
                }

                // Continue has the never type - it diverges
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Continue,
                    ty: Type::NEVER,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::NEVER))
            }

            InstData::Ret(inner) => {
                self.analyze_return(air, inner.as_ref().copied(), inst.span, ctx)
            }

            InstData::Block { extra_start, len } => {
                self.analyze_block(air, *extra_start, *len, inst.span, ctx)
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_control_flow called with non-control-flow instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }

    /// ADR-0079 follow-up: analyze a `comptime if cond { … } else { … }`
    /// by evaluating `cond` at comptime and emitting only the chosen
    /// branch's runtime AIR. The discarded branch is never analyzed,
    /// so it can reference shapes that don't apply to the surrounding
    /// type (e.g. `@uninit(Self)` in the struct branch when `Self` is
    /// an enum). The condition itself contributes no runtime AIR.
    fn analyze_comptime_branch(
        &mut self,
        air: &mut Air,
        cond: InstRef,
        then_block: InstRef,
        else_block: Option<InstRef>,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Evaluate the condition at comptime. We use the
        // heap-preserving evaluator so callers nested inside an outer
        // `comptime_unroll for` keep their loop binding intact.
        let cond_val = {
            let prev_steps = self.comptime_steps_used;
            self.comptime_steps_used = 0;
            let mut locals = ctx.comptime_value_vars.clone();
            let v = self.evaluate_comptime_inst(cond, &mut locals, ctx, span)?;
            self.comptime_steps_used = prev_steps;
            v
        };
        let chosen = match cond_val {
            crate::sema::context::ConstValue::Bool(true) => Some(then_block),
            crate::sema::context::ConstValue::Bool(false) => else_block,
            other => {
                return Err(CompileError::new(
                    ErrorKind::ComptimeEvaluationFailed {
                        reason: format!(
                            "`comptime if` condition must evaluate to a bool at compile time, got {:?}",
                            other
                        ),
                    },
                    span,
                ));
            }
        };
        match chosen {
            Some(branch) => {
                ctx.push_scope();
                let result = self.analyze_inst(air, branch, ctx)?;
                ctx.pop_scope();
                Ok(result)
            }
            None => {
                // `comptime if cond {…}` (no else) where cond is false
                // produces unit, just like a runtime if without else.
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty: Type::UNIT,
                    span,
                });
                Ok(AnalysisResult::new(air_ref, Type::UNIT))
            }
        }
    }

    /// Analyze a branch (if-else) expression.
    #[allow(clippy::too_many_arguments)]
    fn analyze_branch(
        &mut self,
        air: &mut Air,
        cond: InstRef,
        then_block: InstRef,
        else_block: Option<InstRef>,
        is_comptime: bool,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // ADR-0079 follow-up: `comptime if cond { … } else { … }` —
        // evaluate `cond` at comptime and emit *only* the chosen
        // branch's runtime AIR. The discarded branch never gets
        // analyzed (so it can reference shapes that don't apply to
        // the surrounding type — e.g. `@uninit(Self)` inside the
        // struct-only branch when `Self` is enum). The condition
        // itself contributes no runtime AIR.
        if is_comptime {
            return self.analyze_comptime_branch(air, cond, then_block, else_block, span, ctx);
        }

        // Condition must be bool
        let cond_result = self.analyze_inst(air, cond, ctx)?;

        if let Some(else_b) = else_block {
            // Save move state before entering branches.
            let saved_moves = ctx.moved_vars.clone();

            // Analyze then branch with its own scope
            ctx.push_scope();
            let then_result = self.analyze_inst(air, then_block, ctx)?;
            let then_type = then_result.ty;
            let then_span = self.rir.get(then_block).span;
            ctx.pop_scope();

            // Capture then-branch's move state
            let then_moves = ctx.moved_vars.clone();

            // Restore to saved state before analyzing else branch
            ctx.moved_vars = saved_moves;

            // Analyze else branch with its own scope
            ctx.push_scope();
            let else_result = self.analyze_inst(air, else_b, ctx)?;
            let else_type = else_result.ty;
            let else_span = self.rir.get(else_b).span;
            ctx.pop_scope();

            // Capture else-branch's move state
            let else_moves = ctx.moved_vars.clone();

            // Merge move states from both branches.
            ctx.merge_branch_moves(
                then_moves,
                else_moves,
                then_type.is_never(),
                else_type.is_never(),
            );

            // Compute the unified result type using never type coercion
            let result_type = match (then_type.is_never(), else_type.is_never()) {
                (true, true) => Type::NEVER,
                (true, false) => else_type,
                (false, true) => then_type,
                (false, false) => {
                    // Neither diverges - types must match exactly
                    if then_type != else_type && !then_type.is_error() && !else_type.is_error() {
                        return Err(CompileError::type_mismatch(
                            then_type.name().to_string(),
                            else_type.name().to_string(),
                            else_span,
                        )
                        .with_label(format!("this is of type `{}`", then_type.name()), then_span)
                        .with_note("if and else branches must have compatible types"));
                    }
                    then_type
                }
            };

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Branch {
                    cond: cond_result.air_ref,
                    then_value: then_result.air_ref,
                    else_value: Some(else_result.air_ref),
                },
                ty: result_type,
                span,
            });
            Ok(AnalysisResult::new(air_ref, result_type))
        } else {
            // No else branch - result is Unit
            // The then branch must have unit type (spec 4.6:5)

            // Save move state before entering then-branch.
            let saved_moves = ctx.moved_vars.clone();

            ctx.push_scope();
            let then_result = self.analyze_inst(air, then_block, ctx)?;
            ctx.pop_scope();

            // Check that the then branch has unit type (or Never/Error)
            let then_type = then_result.ty;
            if then_type != Type::UNIT && !then_type.is_never() && !then_type.is_error() {
                return Err(CompileError::type_mismatch(
                    "()".to_string(),
                    then_type.name().to_string(),
                    self.rir.get(then_block).span,
                )
                .with_help(
                    "if expressions without else must have unit type; \
                     consider adding an else branch or making the body return ()",
                ));
            }

            // Capture then-branch's move state
            let then_moves = ctx.moved_vars.clone();

            // For if-without-else:
            if then_type.is_never() {
                // Then-branch diverges - code after if only runs if cond was false
                ctx.moved_vars = saved_moves;
            } else {
                // Then-branch doesn't diverge - merge moves (union semantics).
                ctx.merge_branch_moves(
                    then_moves,
                    saved_moves,
                    false, // then doesn't diverge
                    false, // "else" (empty) doesn't diverge
                );
            }

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Branch {
                    cond: cond_result.air_ref,
                    then_value: then_result.air_ref,
                    else_value: None,
                },
                ty: Type::UNIT,
                span,
            });
            Ok(AnalysisResult::new(air_ref, Type::UNIT))
        }
    }

    /// Analyze a while loop.
    fn analyze_while_loop(
        &mut self,
        air: &mut Air,
        cond: InstRef,
        body: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // While loop: condition must be bool, result is Unit
        let cond_result = self.analyze_inst(air, cond, ctx)?;

        // Analyze body with its own scope
        ctx.push_scope();
        ctx.loop_depth += 1;
        let body_result = self.analyze_inst(air, body, ctx)?;
        ctx.loop_depth -= 1;
        ctx.pop_scope();

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Loop {
                cond: cond_result.air_ref,
                body: body_result.air_ref,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
    }

    /// Analyze a for-in loop.
    ///
    /// Desugars `for x in @range(...) { body }` into a while loop:
    /// ```text
    /// {
    ///     let __counter = start;
    ///     while __counter < end {
    ///         let x = __counter;
    ///         body;
    ///         __counter = __counter + stride;
    ///     }
    /// }
    /// ```
    fn analyze_for_loop(
        &mut self,
        air: &mut Air,
        head: ForLoopHead,
        iterable: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Check if the iterable is a @range(...) intrinsic
        let iterable_inst = self.rir.get(iterable);
        match &iterable_inst.data {
            InstData::Intrinsic {
                name,
                args_start,
                args_len,
            } if *name == self.known.range => {
                let args = self.rir.get_inst_refs(*args_start, *args_len);
                self.analyze_range_for_loop(air, &head, &args, iterable_inst.span, ctx)
            }
            _ => {
                // Not @range — peek at the iterable's type (without moving)
                // to dispatch.
                let iterable_span = iterable_inst.span;
                let peek_ty = self.peek_inst_type(iterable, ctx);
                if matches!(peek_ty.map(|t| t.kind()), Some(TypeKind::Vec(_))) {
                    // ADR-0066 Phase 9: for-each over Vec(T) lowers to
                    // iteration over a borrowed slice view. The receiver
                    // must be a place; analyze it for value, then undo the
                    // move (matches slice borrowing semantics).
                    if head.is_mut {
                        return Err(CompileError::type_mismatch(
                            "for-each over a Vec (immutable form only)".to_string(),
                            "mut binding".to_string(),
                            iterable_span,
                        ));
                    }
                    let root_var = self.extract_root_variable(iterable);
                    let iter_res = self.analyze_inst(air, iterable, ctx)?;
                    if let Some(var) = root_var {
                        ctx.moved_vars.remove(&var);
                    }
                    let elem_ty = match iter_res.ty.kind() {
                        TypeKind::Vec(id) => self.type_pool.vec_def(id),
                        _ => unreachable!(),
                    };
                    let slice_id = self.type_pool.intern_slice_from_type(elem_ty);
                    let slice_ty = Type::new_slice(slice_id);
                    let slice_air = air.add_inst(AirInst {
                        data: AirInstData::MakeSlice {
                            base: iter_res.air_ref,
                            lo: None,
                            hi: None,
                            is_mut: false,
                        },
                        ty: slice_ty,
                        span: iterable_span,
                    });
                    return self.analyze_slice_for_loop(air, &head, slice_air, slice_ty, ctx);
                }
                let iterable_result = self.analyze_inst(air, iterable, ctx)?;
                let iterable_type = iterable_result.ty;

                if let Some(array_type_id) = iterable_type.as_array() {
                    let (elem_type, array_len) = self.type_pool.array_def(array_type_id);
                    let is_copy = self.is_type_copy(elem_type);

                    let source = ArraySource {
                        air_ref: iterable_result.air_ref,
                        ty: iterable_type,
                        elem_ty: elem_type,
                        len: array_len,
                        is_copy,
                    };
                    self.analyze_array_for_loop(air, &head, &source, ctx)
                } else if matches!(
                    iterable_type.kind(),
                    TypeKind::Slice(_) | TypeKind::MutSlice(_)
                ) {
                    // ADR-0064 phase 8: iterate over a slice. The mutable
                    // form (which would yield `MutRef(T)`) requires the
                    // deref-assignment operator from ADR-0062 phase 8 and
                    // is deferred — for now we only support immutable
                    // iteration that copies each element by value.
                    if head.is_mut {
                        return Err(CompileError::type_mismatch(
                            "for-each over a slice (immutable form only)".to_string(),
                            "mut binding".to_string(),
                            iterable_span,
                        ));
                    }
                    self.analyze_slice_for_loop(
                        air,
                        &head,
                        iterable_result.air_ref,
                        iterable_type,
                        ctx,
                    )
                } else {
                    Err(CompileError::type_mismatch(
                        "array or @range(...)".to_string(),
                        iterable_type.name().to_string(),
                        iterable_span,
                    ))
                }
            }
        }
    }

    /// Desugar `for x in @range(...) { body }` into a while loop in AIR.
    fn analyze_range_for_loop(
        &mut self,
        air: &mut Air,
        head: &ForLoopHead,
        range_args: &[InstRef],
        range_span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let ForLoopHead {
            binding,
            is_mut,
            body,
            span,
        } = *head;
        // Parse @range arguments: @range(end), @range(start, end), @range(start, end, stride)
        let (start_ref, end_ref, stride_ref) = match range_args.len() {
            1 => (None, range_args[0], None),
            2 => (Some(range_args[0]), range_args[1], None),
            3 => (Some(range_args[0]), range_args[1], Some(range_args[2])),
            _ => {
                return Err(CompileError::new(
                    ErrorKind::WrongArgumentCount {
                        expected: 1, // @range takes 1-3 args
                        found: range_args.len(),
                    },
                    range_span,
                ));
            }
        };

        // Analyze the end bound — this determines the loop variable type
        let end_result = self.analyze_inst(air, end_ref, ctx)?;
        let iter_type = end_result.ty;

        // Validate that the type is an integer type
        if !iter_type.is_integer() {
            return Err(CompileError::type_mismatch(
                "integer type".to_string(),
                format!("{}", iter_type),
                range_span,
            ));
        }

        // Analyze start (default: 0)
        let start_air = if let Some(start_ref) = start_ref {
            let result = self.analyze_inst(air, start_ref, ctx)?;
            if result.ty != iter_type {
                return Err(CompileError::type_mismatch(
                    format!("{}", iter_type),
                    format!("{}", result.ty),
                    range_span,
                ));
            }
            result.air_ref
        } else {
            // Default start: 0
            air.add_inst(AirInst {
                data: AirInstData::Const(0),
                ty: iter_type,
                span,
            })
        };

        // Analyze stride (default: 1)
        let stride_air = if let Some(stride_ref) = stride_ref {
            let result = self.analyze_inst(air, stride_ref, ctx)?;
            if result.ty != iter_type {
                return Err(CompileError::type_mismatch(
                    format!("{}", iter_type),
                    format!("{}", result.ty),
                    range_span,
                ));
            }
            result.air_ref
        } else {
            // Default stride: 1
            air.add_inst(AirInst {
                data: AirInstData::Const(1),
                ty: iter_type,
                span,
            })
        };

        // Open a scope for the entire for-loop (counter variable lives here)
        ctx.push_scope();

        // Allocate a slot for the hidden counter variable
        let counter_slot = ctx.next_slot;
        ctx.next_slot += 1;

        // Emit StorageLive + Alloc for counter
        let counter_storage_live = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot: counter_slot },
            ty: iter_type,
            span,
        });
        let counter_alloc = air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot: counter_slot,
                init: start_air,
            },
            ty: Type::UNIT,
            span,
        });

        // Build the condition: __counter < end
        let counter_load_for_cond = air.add_inst(AirInst {
            data: AirInstData::Load { slot: counter_slot },
            ty: iter_type,
            span,
        });
        let cond_ref = air.add_inst(AirInst {
            data: AirInstData::Bin(BinOp::Lt, counter_load_for_cond, end_result.air_ref),
            ty: Type::BOOL,
            span,
        });

        // Build the loop body:
        // 1. let binding = __counter  (or let mut binding = __counter)
        // 2. user body
        // 3. __counter = __counter + stride

        // Open body scope
        ctx.push_scope();
        ctx.loop_depth += 1;

        // Allocate slot for the user binding variable
        let binding_slot = ctx.next_slot;
        ctx.next_slot += 1;

        // Register the user's loop variable
        ctx.insert_local(
            binding,
            LocalVar {
                slot: binding_slot,
                ty: iter_type,
                is_mut,
                span,
                allow_unused: false,
            },
        );

        // Emit StorageLive + Alloc for binding: let x = __counter
        let counter_load_for_binding = air.add_inst(AirInst {
            data: AirInstData::Load { slot: counter_slot },
            ty: iter_type,
            span,
        });
        let binding_storage_live = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot: binding_slot },
            ty: iter_type,
            span,
        });
        let binding_alloc = air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot: binding_slot,
                init: counter_load_for_binding,
            },
            ty: Type::UNIT,
            span,
        });

        // Increment counter BEFORE user body so `continue` doesn't skip the increment.
        // Desugaring: let x = __counter; __counter += stride; <body>
        let counter_load_for_inc = air.add_inst(AirInst {
            data: AirInstData::Load { slot: counter_slot },
            ty: iter_type,
            span,
        });
        let incremented = air.add_inst(AirInst {
            data: AirInstData::Bin(BinOp::Add, counter_load_for_inc, stride_air),
            ty: iter_type,
            span,
        });
        let counter_store = air.add_inst(AirInst {
            data: AirInstData::Store {
                slot: counter_slot,
                value: incremented,
                had_live_value: true,
            },
            ty: Type::UNIT,
            span,
        });

        // Analyze the user's body
        let body_result = self.analyze_inst(air, body, ctx)?;

        ctx.loop_depth -= 1;
        self.check_unused_locals_in_current_scope(ctx);
        ctx.pop_scope();

        // Build the body block: [binding_storage_live, binding_alloc, counter_store, body]
        // The counter increment is before the user body so `continue` doesn't skip it.
        let body_stmts_start = air.add_extra(&[
            binding_storage_live.as_u32(),
            binding_alloc.as_u32(),
            counter_store.as_u32(),
        ]);
        let body_block = air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start: body_stmts_start,
                stmts_len: 3,
                value: body_result.air_ref,
            },
            ty: Type::UNIT,
            span,
        });

        // Build the while loop
        let loop_ref = air.add_inst(AirInst {
            data: AirInstData::Loop {
                cond: cond_ref,
                body: body_block,
            },
            ty: Type::UNIT,
            span,
        });

        ctx.pop_scope();

        // Build the outer block: [counter_storage_live, counter_alloc, loop]
        let outer_stmts_start =
            air.add_extra(&[counter_storage_live.as_u32(), counter_alloc.as_u32()]);
        let outer_block = air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start: outer_stmts_start,
                stmts_len: 2,
                value: loop_ref,
            },
            ty: Type::UNIT,
            span,
        });

        Ok(AnalysisResult::new(outer_block, Type::UNIT))
    }

    /// Desugar `for x in arr { body }` into a while loop with array indexing.
    ///
    /// For Copy element types, elements are copied out and the array remains valid.
    /// For non-Copy element types, elements are moved out and the array is consumed;
    /// `break` is forbidden because it would leave un-dropped elements.
    fn analyze_array_for_loop(
        &mut self,
        air: &mut Air,
        head: &ForLoopHead,
        source: &ArraySource,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let ForLoopHead {
            binding,
            is_mut,
            body,
            span,
        } = *head;
        let ArraySource {
            air_ref: arr_air,
            ty: arr_type,
            elem_ty: elem_type,
            len: array_len,
            is_copy,
        } = *source;
        // Open outer scope for temp variables
        ctx.push_scope();

        // Spill array to a temporary slot
        let arr_slot = ctx.next_slot;
        let num_arr_slots = self.abi_slot_count(arr_type);
        ctx.next_slot += num_arr_slots;

        let arr_storage_live = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot: arr_slot },
            ty: arr_type,
            span,
        });
        let arr_alloc = air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot: arr_slot,
                init: arr_air,
            },
            ty: Type::UNIT,
            span,
        });

        // Allocate counter: let mut __i: i32 = 0
        let counter_slot = ctx.next_slot;
        ctx.next_slot += 1;

        let counter_start = air.add_inst(AirInst {
            data: AirInstData::Const(0),
            ty: Type::I32,
            span,
        });
        let counter_storage_live = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot: counter_slot },
            ty: Type::I32,
            span,
        });
        let counter_alloc = air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot: counter_slot,
                init: counter_start,
            },
            ty: Type::UNIT,
            span,
        });

        // Build condition: __i < array_len
        let counter_load_for_cond = air.add_inst(AirInst {
            data: AirInstData::Load { slot: counter_slot },
            ty: Type::I32,
            span,
        });
        let len_const = air.add_inst(AirInst {
            data: AirInstData::Const(array_len),
            ty: Type::I32,
            span,
        });
        let cond_ref = air.add_inst(AirInst {
            data: AirInstData::Bin(BinOp::Lt, counter_load_for_cond, len_const),
            ty: Type::BOOL,
            span,
        });

        // Build loop body
        ctx.push_scope();
        ctx.loop_depth += 1;

        // For non-copy element types, forbid break (would leave elements unconsumed)
        let old_forbid_break = ctx.forbid_break.take();
        if !is_copy {
            ctx.forbid_break = Some(elem_type.name().to_string());
        }

        // let x = arr[__i]
        let binding_slot = ctx.next_slot;
        ctx.next_slot += 1;

        ctx.insert_local(
            binding,
            LocalVar {
                slot: binding_slot,
                ty: elem_type,
                is_mut,
                span,
                allow_unused: false,
            },
        );

        // Load index for element access
        let counter_load_for_idx = air.add_inst(AirInst {
            data: AirInstData::Load { slot: counter_slot },
            ty: Type::I32,
            span,
        });

        // Read arr[__i] via PlaceRead
        let place_ref = air.make_place(
            AirPlaceBase::Local(arr_slot),
            std::iter::once(AirProjection::Index {
                array_type: arr_type,
                index: counter_load_for_idx,
            }),
        );
        let elem_read = air.add_inst(AirInst {
            data: AirInstData::PlaceRead { place: place_ref },
            ty: elem_type,
            span,
        });

        // StorageLive + Alloc for binding
        let binding_storage_live = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot: binding_slot },
            ty: elem_type,
            span,
        });
        let binding_alloc = air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot: binding_slot,
                init: elem_read,
            },
            ty: Type::UNIT,
            span,
        });

        // Increment counter before body (so continue doesn't skip it)
        let counter_load_for_inc = air.add_inst(AirInst {
            data: AirInstData::Load { slot: counter_slot },
            ty: Type::I32,
            span,
        });
        let one_const = air.add_inst(AirInst {
            data: AirInstData::Const(1),
            ty: Type::I32,
            span,
        });
        let incremented = air.add_inst(AirInst {
            data: AirInstData::Bin(BinOp::Add, counter_load_for_inc, one_const),
            ty: Type::I32,
            span,
        });
        let counter_store = air.add_inst(AirInst {
            data: AirInstData::Store {
                slot: counter_slot,
                value: incremented,
                had_live_value: true,
            },
            ty: Type::UNIT,
            span,
        });

        // Analyze user body
        let body_result = self.analyze_inst(air, body, ctx)?;

        ctx.loop_depth -= 1;
        ctx.forbid_break = old_forbid_break;
        self.check_unused_locals_in_current_scope(ctx);
        ctx.pop_scope();

        // Build body block: [binding_storage_live, binding_alloc, counter_store, body]
        let body_stmts_start = air.add_extra(&[
            binding_storage_live.as_u32(),
            binding_alloc.as_u32(),
            counter_store.as_u32(),
        ]);
        let body_block = air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start: body_stmts_start,
                stmts_len: 3,
                value: body_result.air_ref,
            },
            ty: Type::UNIT,
            span,
        });

        // Build while loop
        let loop_ref = air.add_inst(AirInst {
            data: AirInstData::Loop {
                cond: cond_ref,
                body: body_block,
            },
            ty: Type::UNIT,
            span,
        });

        ctx.pop_scope();

        // Build outer block: [arr_storage_live, arr_alloc, counter_storage_live, counter_alloc, loop]
        let outer_stmts_start = air.add_extra(&[
            arr_storage_live.as_u32(),
            arr_alloc.as_u32(),
            counter_storage_live.as_u32(),
            counter_alloc.as_u32(),
        ]);
        let outer_block = air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start: outer_stmts_start,
                stmts_len: 4,
                value: loop_ref,
            },
            ty: Type::UNIT,
            span,
        });

        Ok(AnalysisResult::new(outer_block, Type::UNIT))
    }

    /// ADR-0064 phase 8: desugar `for x in s { body }` over a slice into
    /// a counter-driven loop that reads each element via the slice
    /// indexing intrinsic.
    fn analyze_slice_for_loop(
        &mut self,
        air: &mut Air,
        head: &ForLoopHead,
        slice_air: AirRef,
        slice_type: Type,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let ForLoopHead {
            binding,
            body,
            span,
            ..
        } = *head;
        let elem_ty = match slice_type.kind() {
            TypeKind::Slice(id) => self.type_pool.slice_def(id),
            TypeKind::MutSlice(id) => self.type_pool.mut_slice_def(id),
            _ => unreachable!("analyze_slice_for_loop called with non-slice type"),
        };

        if !self.is_type_copy(elem_ty) {
            return Err(CompileError::new(
                ErrorKind::MoveOutOfIndex {
                    element_type: self.format_type_name(elem_ty),
                },
                span,
            ));
        }

        ctx.push_scope();

        // Spill the slice value to a slot.
        let slice_slot = ctx.next_slot;
        ctx.next_slot += self.abi_slot_count(slice_type);
        let slice_storage_live = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot: slice_slot },
            ty: slice_type,
            span,
        });
        let slice_alloc = air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot: slice_slot,
                init: slice_air,
            },
            ty: Type::UNIT,
            span,
        });

        // Counter at slot+1, type usize.
        let counter_slot = ctx.next_slot;
        ctx.next_slot += 1;
        let zero = air.add_inst(AirInst {
            data: AirInstData::Const(0),
            ty: Type::USIZE,
            span,
        });
        let counter_storage_live = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot: counter_slot },
            ty: Type::USIZE,
            span,
        });
        let counter_alloc = air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot: counter_slot,
                init: zero,
            },
            ty: Type::UNIT,
            span,
        });

        // Condition: counter < slice.len()
        let counter_load_for_cond = air.add_inst(AirInst {
            data: AirInstData::Load { slot: counter_slot },
            ty: Type::USIZE,
            span,
        });
        let slice_load_for_len = air.add_inst(AirInst {
            data: AirInstData::Load { slot: slice_slot },
            ty: slice_type,
            span,
        });
        let len_intrinsic_name = self.interner.get_or_intern("slice_len");
        let len_args_start = air.add_extra(&[slice_load_for_len.as_u32()]);
        let len_call = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: len_intrinsic_name,
                args_start: len_args_start,
                args_len: 1,
            },
            ty: Type::USIZE,
            span,
        });
        let cond_ref = air.add_inst(AirInst {
            data: AirInstData::Bin(BinOp::Lt, counter_load_for_cond, len_call),
            ty: Type::BOOL,
            span,
        });

        ctx.push_scope();
        ctx.loop_depth += 1;

        let binding_slot = ctx.next_slot;
        ctx.next_slot += 1;
        ctx.insert_local(
            binding,
            LocalVar {
                slot: binding_slot,
                ty: elem_ty,
                is_mut: false,
                span,
                allow_unused: false,
            },
        );

        // elem = @slice_index_read(slice, counter)
        let counter_load_for_idx = air.add_inst(AirInst {
            data: AirInstData::Load { slot: counter_slot },
            ty: Type::USIZE,
            span,
        });
        let slice_load_for_read = air.add_inst(AirInst {
            data: AirInstData::Load { slot: slice_slot },
            ty: slice_type,
            span,
        });
        let read_name = self.interner.get_or_intern("slice_index_read");
        let read_args_start =
            air.add_extra(&[slice_load_for_read.as_u32(), counter_load_for_idx.as_u32()]);
        let elem_read = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: read_name,
                args_start: read_args_start,
                args_len: 2,
            },
            ty: elem_ty,
            span,
        });

        let binding_storage_live = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot: binding_slot },
            ty: elem_ty,
            span,
        });
        let binding_alloc = air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot: binding_slot,
                init: elem_read,
            },
            ty: Type::UNIT,
            span,
        });

        // counter += 1
        let counter_load_for_inc = air.add_inst(AirInst {
            data: AirInstData::Load { slot: counter_slot },
            ty: Type::USIZE,
            span,
        });
        let one = air.add_inst(AirInst {
            data: AirInstData::Const(1),
            ty: Type::USIZE,
            span,
        });
        let inc = air.add_inst(AirInst {
            data: AirInstData::Bin(BinOp::Add, counter_load_for_inc, one),
            ty: Type::USIZE,
            span,
        });
        let counter_store = air.add_inst(AirInst {
            data: AirInstData::Store {
                slot: counter_slot,
                value: inc,
                had_live_value: true,
            },
            ty: Type::UNIT,
            span,
        });

        let body_result = self.analyze_inst(air, body, ctx)?;

        ctx.loop_depth -= 1;
        self.check_unused_locals_in_current_scope(ctx);
        ctx.pop_scope();

        let body_stmts_start = air.add_extra(&[
            binding_storage_live.as_u32(),
            binding_alloc.as_u32(),
            counter_store.as_u32(),
        ]);
        let body_block = air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start: body_stmts_start,
                stmts_len: 3,
                value: body_result.air_ref,
            },
            ty: Type::UNIT,
            span,
        });
        let loop_ref = air.add_inst(AirInst {
            data: AirInstData::Loop {
                cond: cond_ref,
                body: body_block,
            },
            ty: Type::UNIT,
            span,
        });

        ctx.pop_scope();

        let outer_stmts_start = air.add_extra(&[
            slice_storage_live.as_u32(),
            slice_alloc.as_u32(),
            counter_storage_live.as_u32(),
            counter_alloc.as_u32(),
        ]);
        let outer_block = air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start: outer_stmts_start,
                stmts_len: 4,
                value: loop_ref,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(outer_block, Type::UNIT))
    }

    /// Analyze an infinite loop.
    fn analyze_infinite_loop(
        &mut self,
        air: &mut Air,
        body: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Infinite loop: `loop { body }` - always produces Never type

        ctx.push_scope();
        ctx.loop_depth += 1;
        let body_result = self.analyze_inst(air, body, ctx)?;
        ctx.loop_depth -= 1;
        ctx.pop_scope();

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::InfiniteLoop {
                body: body_result.air_ref,
            },
            ty: Type::NEVER,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::NEVER))
    }

    /// ADR-0079 Phase 3: analyze a match-arm body with the
    /// per-iteration comptime binding installed, if this arm came
    /// from `comptime_unroll for v in …` expansion. Otherwise just
    /// delegates to `analyze_inst`.
    fn analyze_arm_body(
        &mut self,
        air: &mut Air,
        body: gruel_rir::InstRef,
        arm_idx: usize,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let binding = ctx.unroll_arm_bindings.get(&arm_idx).cloned();
        let Some(b) = binding else {
            return self.analyze_inst(air, body, ctx);
        };
        let prev = ctx.comptime_value_vars.insert(b.binding, b.value);
        let res = self.analyze_inst(air, body, ctx);
        match prev {
            Some(v) => {
                ctx.comptime_value_vars.insert(b.binding, v);
            }
            None => {
                ctx.comptime_value_vars.remove(&b.binding);
            }
        }
        res
    }

    /// ADR-0079 Phase 3: expand any `comptime_unroll for v in ...`
    /// arm templates into one regular arm per element of the iterable.
    /// Non-template arms pass through unchanged.
    ///
    /// The expansion looks at `@type_info(Self).variants`-shaped
    /// comptime arrays — each element is a `VariantInfo` struct with
    /// at least a `name` field. For each element, we look up the
    /// matching enum variant on `scrutinee_type` and synthesize a
    /// catch-all variant pattern (`Self::A`, `Self::B(_)`, or
    /// `Self::C { .. }` depending on shape) plus the body. The
    /// per-iteration comptime binding (`v` in the user's source) is
    /// recorded into `ctx.unroll_arm_bindings` keyed by output arm
    /// index; the match-arm body analyzer reads it back to push the
    /// binding into `ctx.comptime_value_vars` before recursing.
    fn expand_unroll_arms(
        &mut self,
        raw: Vec<(RirPattern, InstRef)>,
        scrutinee_type: Type,
        ctx: &mut AnalysisContext,
        span: Span,
    ) -> CompileResult<Vec<(RirPattern, InstRef)>> {
        use crate::sema::context::{ComptimeHeapItem, ConstValue, UnrollArmBinding};

        // Quick check: if no arm is an unroll template, return raw unchanged.
        let has_unroll = raw
            .iter()
            .any(|(p, _)| matches!(p, RirPattern::ComptimeUnrollArm { .. }));
        if !has_unroll {
            return Ok(raw);
        }

        let enum_id = scrutinee_type.as_enum().ok_or_else(|| {
            CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "`comptime_unroll for v in ...` arm requires an enum scrutinee".into(),
                },
                span,
            )
        })?;

        let mut expanded: Vec<(RirPattern, InstRef)> = Vec::with_capacity(raw.len());
        for (pattern, body) in raw {
            let RirPattern::ComptimeUnrollArm {
                binding,
                iterable,
                span: arm_span,
            } = pattern
            else {
                expanded.push((pattern, body));
                continue;
            };

            // Evaluate the iterable at comptime.
            let iter_val = self.evaluate_comptime_block(iterable, ctx, arm_span)?;
            let elements = match iter_val {
                ConstValue::Array(heap_idx) => match &self.comptime_heap[heap_idx as usize] {
                    ComptimeHeapItem::Array(elems) => elems.clone(),
                    _ => return Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason:
                                "`comptime_unroll for` iterable did not resolve to a comptime array"
                                    .into(),
                        },
                        arm_span,
                    )),
                },
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason:
                                "`comptime_unroll for` iterable did not resolve to a comptime array"
                                    .into(),
                        },
                        arm_span,
                    ));
                }
            };

            for el in elements {
                let variant_name = self.extract_variant_info_name(el, arm_span)?;
                let variant_idx = self
                    .type_pool
                    .enum_def(enum_id)
                    .variants
                    .iter()
                    .position(|v| v.name == variant_name)
                    .ok_or_else(|| {
                        CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: format!(
                                    "comptime_unroll for: variant `{}` not found on enum `{}`",
                                    variant_name,
                                    self.type_pool.enum_def(enum_id).name
                                ),
                            },
                            arm_span,
                        )
                    })? as u32;

                let synthesized = self.synthesize_variant_pattern(enum_id, variant_idx, arm_span);

                let arm_index = expanded.len();
                ctx.unroll_arm_bindings
                    .insert(arm_index, UnrollArmBinding { binding, value: el });
                expanded.push((synthesized, body));
            }
        }
        Ok(expanded)
    }

    /// Pull the `name` field out of a comptime `VariantInfo` value
    /// (an array element of `@type_info(Self).variants`). Errors if
    /// the value is not a struct or has no string `name` field.
    fn extract_variant_info_name(
        &self,
        val: crate::sema::context::ConstValue,
        span: Span,
    ) -> CompileResult<String> {
        use crate::sema::context::{ComptimeHeapItem, ConstValue};
        let ConstValue::Struct(heap_idx) = val else {
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "comptime_unroll for: iterable element is not a struct".into(),
                },
                span,
            ));
        };
        let (struct_id, fields) = match &self.comptime_heap[heap_idx as usize] {
            ComptimeHeapItem::Struct { struct_id, fields } => (*struct_id, fields.clone()),
            _ => {
                return Err(CompileError::new(
                    ErrorKind::ComptimeEvaluationFailed {
                        reason: "comptime_unroll for: iterable element is not a struct".into(),
                    },
                    span,
                ));
            }
        };
        let struct_def = self.type_pool.struct_def(struct_id);
        let name_idx = struct_def
            .fields
            .iter()
            .position(|f| f.name == "name")
            .ok_or_else(|| {
                CompileError::new(
                    ErrorKind::ComptimeEvaluationFailed {
                        reason: "comptime_unroll for: iterable element has no `name` field".into(),
                    },
                    span,
                )
            })?;
        let name_val = fields[name_idx];
        let ConstValue::ComptimeStr(name_idx) = name_val else {
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "comptime_unroll for: variant `name` is not a comptime string".into(),
                },
                span,
            ));
        };
        match &self.comptime_heap[name_idx as usize] {
            ComptimeHeapItem::String(s) => Ok(s.clone()),
            _ => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "comptime_unroll for: variant `name` is not a string".into(),
                },
                span,
            )),
        }
    }

    /// Synthesize a catch-all variant pattern for the given variant.
    /// Unit variants → `Path`; tuple variants → `DataVariant` with
    /// all-wildcard bindings; struct variants → `StructVariant` with
    /// rest sentinel.
    fn synthesize_variant_pattern(
        &mut self,
        enum_id: crate::types::EnumId,
        variant_idx: u32,
        span: Span,
    ) -> RirPattern {
        let enum_def = self.type_pool.enum_def(enum_id);
        let enum_name = self.interner.get_or_intern(&enum_def.name);
        let variant = &enum_def.variants[variant_idx as usize];
        let variant_name = self.interner.get_or_intern(&variant.name);
        if variant.fields.is_empty() {
            return RirPattern::Path {
                module: None,
                type_name: enum_name,
                variant: variant_name,
                span,
            };
        }
        if variant.is_struct_variant() {
            // `..` rest binding: name == "..".
            let rest_marker = self.interner.get_or_intern_static("..");
            return RirPattern::StructVariant {
                module: None,
                type_name: enum_name,
                variant: variant_name,
                field_bindings: vec![gruel_rir::RirStructPatternBinding {
                    field_name: rest_marker,
                    binding: gruel_rir::RirPatternBinding {
                        is_wildcard: true,
                        is_mut: false,
                        name: Some(rest_marker),
                        sub_pattern: None,
                    },
                }],
                span,
            };
        }
        // Tuple variant: emit a wildcard for each field.
        let bindings = (0..variant.fields.len())
            .map(|_| gruel_rir::RirPatternBinding {
                is_wildcard: true,
                is_mut: false,
                name: None,
                sub_pattern: None,
            })
            .collect();
        RirPattern::DataVariant {
            module: None,
            type_name: enum_name,
            variant: variant_name,
            bindings,
            span,
        }
    }

    /// Analyze a match expression.
    fn analyze_match(
        &mut self,
        air: &mut Air,
        scrutinee: InstRef,
        arms_start: u32,
        arms_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Analyze the scrutinee to determine its type
        let scrutinee_result = self.analyze_inst(air, scrutinee, ctx)?;
        let scrutinee_type = scrutinee_result.ty;

        // Validate that we can match on this type. Structs (including
        // tuples) are allowed because ADR-0051 `RirPattern::Tuple` /
        // `Struct` arms lower through sema + CFG cascading dispatch.
        // `!` (Type::NEVER) is allowed too — ADR-0052 Phase 5 permits
        // zero-arm matches on uninhabited scrutinees.
        let is_struct_like = scrutinee_type.is_struct();
        if !scrutinee_type.is_integer()
            && scrutinee_type != Type::BOOL
            && !scrutinee_type.is_enum()
            && !is_struct_like
            && !scrutinee_type.is_never()
        {
            return Err(CompileError::new(
                ErrorKind::InvalidMatchType(scrutinee_type.name().to_string()),
                span,
            ));
        }

        let raw_arms = self.rir.get_match_arms(arms_start, arms_len);
        // ADR-0079 Phase 3: expand any `comptime_unroll for v in ...`
        // arm templates into one regular arm per element of the
        // iterable. The expansion is sema-side because we need the
        // comptime evaluator and the scrutinee's enum metadata; the
        // expanded arms then flow through the regular validation /
        // reachability machinery below as if the user had written
        // them by hand.
        let arms = self.expand_unroll_arms(raw_arms, scrutinee_type, ctx, span)?;
        if arms.is_empty() {
            // ADR-0052 Phase 5: a zero-arm match is vacuously exhaustive
            // when the scrutinee is uninhabited — either `!` directly or
            // a zero-variant enum. CFG lowers the block with
            // `Terminator::Unreachable`.
            let is_empty_enum = scrutinee_type
                .as_enum()
                .is_some_and(|id| self.type_pool.enum_def(id).variants.is_empty());
            if scrutinee_type.is_never() || is_empty_enum {
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Match {
                        scrutinee: scrutinee_result.air_ref,
                        arms_start: 0,
                        arms_len: 0,
                    },
                    ty: Type::NEVER,
                    span,
                });
                return Ok(AnalysisResult::new(air_ref, Type::NEVER));
            }
            return Err(CompileError::new(ErrorKind::EmptyMatch, span));
        }

        // Collect arm spans so the post-loop reachability check can point
        // at the exact unreachable arm.
        let mut arm_spans: Vec<Span> = Vec::with_capacity(arms.len());

        // Analyze each arm (each arm gets its own scope)
        let mut air_arms = Vec::new();
        let mut result_type: Option<Type> = None;

        for (arm_idx, (pattern, body)) in arms.iter().enumerate() {
            let pattern_span = pattern.span();

            // Cached resolution for enum patterns (set during validation, reused
            // in body analysis and AIR pattern conversion to avoid repeated lookups).
            let mut resolved_enum: Option<(crate::types::EnumId, u32)> = None;

            arm_spans.push(pattern_span);

            // Type-only validation of literal / bool patterns; reachability
            // is handled post-loop via Maranget.
            match pattern {
                RirPattern::Wildcard(_) => {}
                RirPattern::Int(_, _) => {
                    if !scrutinee_type.is_integer() {
                        return Err(CompileError::type_mismatch(
                            scrutinee_type.name().to_string(),
                            "integer".to_string(),
                            pattern_span,
                        ));
                    }
                }
                RirPattern::Bool(_, _) => {
                    if scrutinee_type != Type::BOOL {
                        return Err(CompileError::type_mismatch(
                            scrutinee_type.name().to_string(),
                            "bool".to_string(),
                            pattern_span,
                        ));
                    }
                }
                RirPattern::Path {
                    module,
                    type_name,
                    variant,
                    ..
                } => {
                    // Look up the enum type, potentially through a module or comptime type variable
                    let enum_id = if let Some(module_ref) = module {
                        self.resolve_enum_through_module(*module_ref, *type_name, pattern_span)?
                    } else if let Some(&enum_id) = self.enums.get(type_name) {
                        enum_id
                    } else if let Some(&ty) = ctx.comptime_type_vars.get(type_name) {
                        match ty.kind() {
                            TypeKind::Enum(id) => id,
                            _ => {
                                return Err(CompileError::new(
                                    ErrorKind::UnknownEnumType(
                                        self.interner.resolve(type_name).to_string(),
                                    ),
                                    pattern_span,
                                ));
                            }
                        }
                    } else {
                        return Err(CompileError::new(
                            ErrorKind::UnknownEnumType(
                                self.interner.resolve(type_name).to_string(),
                            ),
                            pattern_span,
                        ));
                    };
                    let enum_def = self.type_pool.enum_def(enum_id);

                    // Check that scrutinee type matches the pattern's enum type
                    if scrutinee_type != Type::new_enum(enum_id) {
                        return Err(CompileError::type_mismatch(
                            scrutinee_type.name().to_string(),
                            enum_def.name.clone(),
                            pattern_span,
                        ));
                    }

                    // Find the variant index
                    let variant_name = self.interner.resolve(variant);
                    let variant_index = enum_def.find_variant(variant_name).ok_or_compile_error(
                        ErrorKind::UnknownVariant {
                            enum_name: enum_def.name.clone(),
                            variant_name: variant_name.to_string(),
                        },
                        pattern_span,
                    )?;

                    resolved_enum = Some((enum_id, variant_index as u32));
                }
                RirPattern::DataVariant {
                    module,
                    type_name,
                    variant,
                    bindings,
                    ..
                } => {
                    // Look up the enum type, including comptime type variable resolution
                    let enum_id = if let Some(module_ref) = module {
                        self.resolve_enum_through_module(*module_ref, *type_name, pattern_span)?
                    } else if let Some(&enum_id) = self.enums.get(type_name) {
                        enum_id
                    } else if let Some(&ty) = ctx.comptime_type_vars.get(type_name) {
                        match ty.kind() {
                            TypeKind::Enum(id) => id,
                            _ => {
                                return Err(CompileError::new(
                                    ErrorKind::UnknownEnumType(
                                        self.interner.resolve(type_name).to_string(),
                                    ),
                                    pattern_span,
                                ));
                            }
                        }
                    } else {
                        return Err(CompileError::new(
                            ErrorKind::UnknownEnumType(
                                self.interner.resolve(type_name).to_string(),
                            ),
                            pattern_span,
                        ));
                    };
                    let enum_def = self.type_pool.enum_def(enum_id);

                    // Check that scrutinee type matches
                    if scrutinee_type != Type::new_enum(enum_id) {
                        return Err(CompileError::type_mismatch(
                            scrutinee_type.name().to_string(),
                            enum_def.name.clone(),
                            pattern_span,
                        ));
                    }

                    // Find the variant
                    let variant_name = self.interner.resolve(variant);
                    let variant_index = enum_def.find_variant(variant_name).ok_or_compile_error(
                        ErrorKind::UnknownVariant {
                            enum_name: enum_def.name.clone(),
                            variant_name: variant_name.to_string(),
                        },
                        pattern_span,
                    )?;

                    // Check binding count matches field count. A `..` rest
                    // marker (emitted by astgen as a wildcard binding named
                    // `..`) must appear last and accounts for any remaining
                    // fields not otherwise listed (ADR-0049 Phase 6).
                    let field_count = enum_def.variants[variant_index].fields.len();
                    let rest_marker = self.interner.get_or_intern_static("..");
                    let has_rest = bindings.iter().any(|b| b.name == Some(rest_marker));
                    if has_rest {
                        // Require `..` at the end — any-position `..` needs
                        // explicit positions-from-end support which is a
                        // follow-up.
                        let rest_at_end =
                            bindings.last().is_some_and(|b| b.name == Some(rest_marker));
                        if !rest_at_end {
                            return Err(CompileError::new(
                                ErrorKind::WrongArgumentCount {
                                    expected: field_count,
                                    found: bindings.len(),
                                },
                                pattern_span,
                            ));
                        }
                        let explicit = bindings.len() - 1; // subtract the rest marker
                        if explicit > field_count {
                            return Err(CompileError::new(
                                ErrorKind::WrongArgumentCount {
                                    expected: field_count,
                                    found: explicit,
                                },
                                pattern_span,
                            ));
                        }
                    } else if bindings.len() != field_count {
                        return Err(CompileError::new(
                            ErrorKind::WrongArgumentCount {
                                expected: field_count,
                                found: bindings.len(),
                            },
                            pattern_span,
                        ));
                    }

                    resolved_enum = Some((enum_id, variant_index as u32));
                }
                RirPattern::StructVariant {
                    module,
                    type_name,
                    variant,
                    field_bindings,
                    ..
                } => {
                    // Look up the enum type, including comptime type variable resolution
                    let enum_id = if let Some(module_ref) = module {
                        self.resolve_enum_through_module(*module_ref, *type_name, pattern_span)?
                    } else if let Some(&enum_id) = self.enums.get(type_name) {
                        enum_id
                    } else if let Some(&ty) = ctx.comptime_type_vars.get(type_name) {
                        match ty.kind() {
                            TypeKind::Enum(id) => id,
                            _ => {
                                return Err(CompileError::new(
                                    ErrorKind::UnknownEnumType(
                                        self.interner.resolve(type_name).to_string(),
                                    ),
                                    pattern_span,
                                ));
                            }
                        }
                    } else {
                        return Err(CompileError::new(
                            ErrorKind::UnknownEnumType(
                                self.interner.resolve(type_name).to_string(),
                            ),
                            pattern_span,
                        ));
                    };
                    let enum_def = self.type_pool.enum_def(enum_id);

                    // Check that scrutinee type matches
                    if scrutinee_type != Type::new_enum(enum_id) {
                        return Err(CompileError::type_mismatch(
                            scrutinee_type.name().to_string(),
                            enum_def.name.clone(),
                            pattern_span,
                        ));
                    }

                    // Find the variant
                    let variant_name = self.interner.resolve(variant);
                    let variant_index = enum_def.find_variant(variant_name).ok_or_compile_error(
                        ErrorKind::UnknownVariant {
                            enum_name: enum_def.name.clone(),
                            variant_name: variant_name.to_string(),
                        },
                        pattern_span,
                    )?;

                    let variant_def = &enum_def.variants[variant_index];

                    // Verify this is a struct variant
                    if !variant_def.is_struct_variant() {
                        return Err(CompileError::type_mismatch(
                            format!(
                                "tuple-style pattern `{}::{}(...)`",
                                enum_def.name, variant_name
                            ),
                            format!(
                                "struct-style pattern `{}::{} {{ ... }}`",
                                enum_def.name, variant_name
                            ),
                            pattern_span,
                        ));
                    }

                    // Check for unknown, duplicate, and missing fields.
                    // Detect and strip a `..` rest marker (ADR-0049 Phase 6):
                    // when present, missing fields are permitted and treated
                    // as wildcards below.
                    let rest_marker_sv = self.interner.get_or_intern_static("..");
                    let sv_has_rest = field_bindings
                        .iter()
                        .any(|fb| fb.field_name == rest_marker_sv);
                    let mut seen_fields: rustc_hash::FxHashSet<_> = {
                        let mut s = rustc_hash::FxHashSet::default();
                        s.reserve(field_bindings.len());
                        s
                    };
                    let qualified_name = format!("{}::{}", enum_def.name, variant_name);
                    for fb in field_bindings {
                        if fb.field_name == rest_marker_sv {
                            continue;
                        }
                        let field_name_str = self.interner.resolve(&fb.field_name);
                        if !seen_fields.insert(fb.field_name) {
                            return Err(CompileError::new(
                                ErrorKind::DuplicateField {
                                    struct_name: qualified_name.clone(),
                                    field_name: field_name_str.to_string(),
                                },
                                pattern_span,
                            ));
                        }
                        if variant_def.find_field(field_name_str).is_none() {
                            return Err(CompileError::new(
                                ErrorKind::UnknownField {
                                    struct_name: qualified_name.clone(),
                                    field_name: field_name_str.to_string(),
                                },
                                pattern_span,
                            ));
                        }
                    }

                    // Check for missing fields (waived when `..` is present;
                    // the rest marker absorbs any unlisted fields).
                    if sv_has_rest {
                        resolved_enum = Some((enum_id, variant_index as u32));
                    } else {
                        let declared_field_count = variant_def.field_names.len();
                        if field_bindings.len() != declared_field_count {
                            // Find which fields are missing
                            let missing: Vec<_> = variant_def
                                .field_names
                                .iter()
                                .filter(|name| {
                                    !field_bindings.iter().any(|fb| {
                                        self.interner.resolve(&fb.field_name) == name.as_str()
                                    })
                                })
                                .cloned()
                                .collect();
                            return Err(CompileError::new(
                                ErrorKind::MissingFields(Box::new(MissingFieldsError {
                                    struct_name: qualified_name,
                                    missing_fields: missing,
                                })),
                                pattern_span,
                            ));
                        }

                        resolved_enum = Some((enum_id, variant_index as u32));
                    }
                }
                // ADR-0051 Phase 4b: validate new top-level arm shapes.
                // Full structural checking (field names, arity, element
                // types) is deferred — sema's current machinery is built
                // around flat bindings and the rewrite is scheduled for
                // Phase 4c once the elaboration layer is gone. Here we
                // only sanity-check that the scrutinee's shape matches
                // the arm kind; element-level type checking piggybacks
                // on body analysis through the introduced bindings.
                RirPattern::Ident { .. } => {}
                RirPattern::Tuple { .. } => {
                    if !is_struct_like {
                        return Err(CompileError::type_mismatch(
                            scrutinee_type.name().to_string(),
                            "tuple".to_string(),
                            pattern_span,
                        ));
                    }
                }
                RirPattern::Struct { type_name, .. } => {
                    if !is_struct_like {
                        return Err(CompileError::type_mismatch(
                            scrutinee_type.name().to_string(),
                            self.interner.resolve(type_name).to_string(),
                            pattern_span,
                        ));
                    }
                }
                // ADR-0079 Phase 3: unroll-arm templates are
                // expanded by `expand_unroll_arms` *before* this
                // loop runs, so an unexpanded one here is an ICE.
                RirPattern::ComptimeUnrollArm { .. } => {
                    return Err(CompileError::new(
                        ErrorKind::InternalError(
                            "comptime_unroll for arm not expanded before validation".into(),
                        ),
                        pattern_span,
                    ));
                }
            }

            // Each arm gets its own scope
            ctx.push_scope();

            // For DataVariant/StructVariant patterns, emit field extractions into the arm scope
            // before analyzing the body. Named bindings become local variables.
            //
            // Collect indexed bindings: (field_index, binding) for each field that needs extraction.
            // DataVariant: bindings are positional (field_index == position).
            // StructVariant: bindings are named, resolved to declaration-order indices.
            // Owned storage for synthesized rest-pad bindings (ADR-0049
            // Phase 6). `..` in a DataVariant pattern expands to wildcard
            // bindings covering the remaining variant-field positions;
            // since these synthetics have no source RirPatternBinding to
            // borrow, we own them here and hand out references below.
            // Owned storage for synthesized rest-pad bindings (ADR-0049
            // Phase 6). `..` in a DataVariant/StructVariant pattern expands
            // to wildcard bindings covering the remaining variant-field
            // positions; since these synthetics have no source
            // RirPatternBinding to borrow, we own them here and hand out
            // references below.
            let rest_marker_sp = self.interner.get_or_intern_static("..");
            let rest_padding: Vec<RirPatternBinding> = match pattern {
                RirPattern::DataVariant { bindings, .. }
                    if bindings
                        .last()
                        .is_some_and(|b| b.name == Some(rest_marker_sp)) =>
                {
                    let (enum_id, variant_index) =
                        resolved_enum.expect("resolved_enum set during validation");
                    let enum_def = self.type_pool.enum_def(enum_id);
                    let field_count = enum_def.variants[variant_index as usize].fields.len();
                    let explicit = bindings.len() - 1;
                    let rest_count = field_count - explicit;
                    (0..rest_count)
                        .map(|_| RirPatternBinding {
                            is_wildcard: true,
                            is_mut: false,
                            name: None,
                            sub_pattern: None,
                        })
                        .collect()
                }
                RirPattern::StructVariant { field_bindings, .. }
                    if field_bindings
                        .iter()
                        .any(|fb| fb.field_name == rest_marker_sp) =>
                {
                    // One wildcard per unlisted variant field.
                    let (enum_id, variant_index) =
                        resolved_enum.expect("resolved_enum set during validation");
                    let enum_def = self.type_pool.enum_def(enum_id);
                    let variant_def = &enum_def.variants[variant_index as usize];
                    let listed: rustc_hash::FxHashSet<&str> = field_bindings
                        .iter()
                        .filter(|fb| fb.field_name != rest_marker_sp)
                        .map(|fb| self.interner.resolve(&fb.field_name))
                        .collect();
                    variant_def
                        .field_names
                        .iter()
                        .filter(|n| !listed.contains(n.as_str()))
                        .map(|_| RirPatternBinding {
                            is_wildcard: true,
                            is_mut: false,
                            name: None,
                            sub_pattern: None,
                        })
                        .collect()
                }
                _ => Vec::new(),
            };
            let indexed_bindings: Option<Vec<(usize, &RirPatternBinding)>> = match pattern {
                RirPattern::DataVariant { bindings, .. } => {
                    if !rest_padding.is_empty() {
                        let explicit = bindings.len() - 1;
                        let mut expanded: Vec<(usize, &RirPatternBinding)> = Vec::new();
                        for (i, b) in bindings.iter().take(explicit).enumerate() {
                            expanded.push((i, b));
                        }
                        for (j, b) in rest_padding.iter().enumerate() {
                            expanded.push((explicit + j, b));
                        }
                        Some(expanded)
                    } else {
                        Some(bindings.iter().enumerate().collect())
                    }
                }
                RirPattern::StructVariant { field_bindings, .. } => {
                    let (enum_id, variant_index) = resolved_enum
                        .expect("resolved_enum must be set for StructVariant patterns");
                    let enum_def = self.type_pool.enum_def(enum_id);
                    let variant_def = &enum_def.variants[variant_index as usize];
                    let listed_indices: rustc_hash::FxHashSet<usize> = field_bindings
                        .iter()
                        .filter(|fb| fb.field_name != rest_marker_sp)
                        .filter_map(|fb| {
                            let name = self.interner.resolve(&fb.field_name);
                            variant_def.find_field(name)
                        })
                        .collect();
                    let mut expanded: Vec<(usize, &RirPatternBinding)> = field_bindings
                        .iter()
                        .filter(|fb| fb.field_name != rest_marker_sp)
                        .map(|fb| {
                            let field_name = self.interner.resolve(&fb.field_name);
                            let idx = variant_def
                                .find_field(field_name)
                                .expect("field name validated during pattern checking");
                            (idx, &fb.binding)
                        })
                        .collect();
                    // Pad with wildcards for any unlisted fields (when `..`
                    // is present, rest_padding has been sized to cover them).
                    let mut rest_iter = rest_padding.iter();
                    for (i, field_name) in variant_def.field_names.iter().enumerate() {
                        if !listed_indices.contains(&i) {
                            let Some(pad) = rest_iter.next() else {
                                // Not a rest match — missing-fields check already
                                // handled this case above.
                                break;
                            };
                            let _ = field_name;
                            expanded.push((i, pad));
                        }
                    }
                    Some(expanded)
                }
                _ => None,
            };

            let body_result = if let Some(indexed_bindings) = indexed_bindings {
                // Reuse the enum_id and variant_index resolved during validation.
                let (enum_id, variant_index) = resolved_enum
                    .expect("resolved_enum must be set for data/struct variant patterns");
                let enum_def = self.type_pool.enum_def(enum_id);
                let field_types: Vec<Type> =
                    enum_def.variants[variant_index as usize].fields.clone();

                let mut storage_lives = Vec::new();
                let mut allocs = Vec::new();

                for (field_index, binding) in &indexed_bindings {
                    let field_ty = field_types[*field_index];

                    // Extract field value from enum payload
                    let field_val = air.add_inst(AirInst {
                        data: AirInstData::EnumPayloadGet {
                            base: scrutinee_result.air_ref,
                            variant_index,
                            field_index: *field_index as u32,
                        },
                        ty: field_ty,
                        span: pattern_span,
                    });

                    // ADR-0051: a binding with a nested sub-pattern recurses
                    // directly using the extracted field value as scrutinee,
                    // rather than materialising a slot for the whole field.
                    if let Some(sub) = &binding.sub_pattern {
                        let mut out = BindingEmission {
                            storage_lives: &mut storage_lives,
                            allocs: &mut allocs,
                        };
                        self.emit_recursive_pattern_bindings(
                            air, field_val, field_ty, sub, ctx, &mut out,
                        );
                        continue;
                    }

                    // Allocate a slot for this binding
                    let slot = ctx.next_slot;
                    ctx.next_slot += 1;

                    let storage_live = air.add_inst(AirInst {
                        data: AirInstData::StorageLive { slot },
                        ty: field_ty,
                        span: pattern_span,
                    });
                    storage_lives.push(storage_live);

                    let alloc = air.add_inst(AirInst {
                        data: AirInstData::Alloc {
                            slot,
                            init: field_val,
                        },
                        ty: Type::UNIT,
                        span: pattern_span,
                    });
                    allocs.push(alloc);

                    // Register named bindings in the arm scope
                    if !binding.is_wildcard
                        && let Some(name_spur) = binding.name
                    {
                        ctx.insert_local(
                            name_spur,
                            LocalVar {
                                slot,
                                ty: field_ty,
                                is_mut: binding.is_mut,
                                span: pattern_span,
                                allow_unused: false,
                            },
                        );
                    }
                }

                // Analyze the arm body (can reference the bound variables)
                let inner_result = self.analyze_arm_body(air, *body, arm_idx, ctx)?;
                let body_type = inner_result.ty;

                // Wrap storage_lives + allocs + body in a Block
                let unit = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty: Type::UNIT,
                    span: pattern_span,
                });
                let allocs_start = air.add_extra(
                    &allocs
                        .iter()
                        .map(|r: &AirRef| r.as_u32())
                        .collect::<Vec<_>>(),
                );
                let inner_block = air.add_inst(AirInst {
                    data: AirInstData::Block {
                        stmts_start: allocs_start,
                        stmts_len: allocs.len() as u32,
                        value: unit,
                    },
                    ty: Type::UNIT,
                    span: pattern_span,
                });

                let sl_start = air.add_extra(
                    &storage_lives
                        .iter()
                        .map(|r: &AirRef| r.as_u32())
                        .collect::<Vec<_>>(),
                );
                let setup_block = air.add_inst(AirInst {
                    data: AirInstData::Block {
                        stmts_start: sl_start,
                        stmts_len: storage_lives.len() as u32,
                        value: inner_block,
                    },
                    ty: Type::UNIT,
                    span: pattern_span,
                });

                // The actual body ref is the setup_block followed by the user body.
                // We wrap them together as: block { stmts: [setup_block], value: inner_result }
                let stmts_start = air.add_extra(&[setup_block.as_u32()]);
                let combined = air.add_inst(AirInst {
                    data: AirInstData::Block {
                        stmts_start,
                        stmts_len: 1,
                        value: inner_result.air_ref,
                    },
                    ty: body_type,
                    span: pattern_span,
                });

                AnalysisResult::new(combined, body_type)
            } else if matches!(
                pattern,
                RirPattern::Ident { .. } | RirPattern::Tuple { .. } | RirPattern::Struct { .. }
            ) {
                // ADR-0051 Phase 4c: introduce bindings for Ident leaves in
                // top-level Tuple / Struct / Ident arm patterns. Walks the
                // pattern tree with FieldGet projections, allocates slots,
                // emits StorageLive / Alloc, registers locals, then
                // analyses the arm body with the bindings in scope. Wraps
                // setup + body in a Block so CFG sees them together.
                // Tuple `..` rest is expanded to wildcards up front so
                // the walk maps positions to struct fields directly.
                let expanded = expanded_tuple_pattern(pattern, &self.type_pool, scrutinee_type);
                let mut storage_lives = Vec::new();
                let mut allocs = Vec::new();
                let mut out = BindingEmission {
                    storage_lives: &mut storage_lives,
                    allocs: &mut allocs,
                };
                self.emit_recursive_pattern_bindings(
                    air,
                    scrutinee_result.air_ref,
                    scrutinee_type,
                    &expanded,
                    ctx,
                    &mut out,
                );

                let inner_result = self.analyze_arm_body(air, *body, arm_idx, ctx)?;
                let body_type = inner_result.ty;

                let unit = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty: Type::UNIT,
                    span: pattern_span,
                });
                let allocs_start = air.add_extra(
                    &allocs
                        .iter()
                        .map(|r: &AirRef| r.as_u32())
                        .collect::<Vec<_>>(),
                );
                let inner_block = air.add_inst(AirInst {
                    data: AirInstData::Block {
                        stmts_start: allocs_start,
                        stmts_len: allocs.len() as u32,
                        value: unit,
                    },
                    ty: Type::UNIT,
                    span: pattern_span,
                });
                let sl_start = air.add_extra(
                    &storage_lives
                        .iter()
                        .map(|r: &AirRef| r.as_u32())
                        .collect::<Vec<_>>(),
                );
                let setup_block = air.add_inst(AirInst {
                    data: AirInstData::Block {
                        stmts_start: sl_start,
                        stmts_len: storage_lives.len() as u32,
                        value: inner_block,
                    },
                    ty: Type::UNIT,
                    span: pattern_span,
                });
                let stmts_start = air.add_extra(&[setup_block.as_u32()]);
                let combined = air.add_inst(AirInst {
                    data: AirInstData::Block {
                        stmts_start,
                        stmts_len: 1,
                        value: inner_result.air_ref,
                    },
                    ty: body_type,
                    span: pattern_span,
                });
                AnalysisResult::new(combined, body_type)
            } else {
                // Non-data/struct variant: analyze body normally
                self.analyze_arm_body(air, *body, arm_idx, ctx)?
            };

            let body_type = body_result.ty;

            // Check for unused pattern bindings before popping the arm scope
            self.check_unused_locals_in_current_scope(ctx);

            ctx.pop_scope();

            // Update result type (handle Never type coercion)
            result_type = Some(match result_type {
                None => body_type,
                Some(prev) => {
                    if prev.is_never() {
                        body_type
                    } else if body_type.is_never() {
                        prev
                    } else if prev != body_type && !prev.is_error() && !body_type.is_error() {
                        return Err(CompileError::type_mismatch(
                            prev.name().to_string(),
                            body_type.name().to_string(),
                            self.rir.get(*body).span,
                        ));
                    } else {
                        prev
                    }
                }
            });

            // Convert pattern to AIR pattern. Expand a top-level tuple `..`
            // rest to wildcards so `lower_pattern` sees a flat element list
            // whose positions align with the scrutinee's struct fields.
            let expanded = expanded_tuple_pattern(pattern, &self.type_pool, scrutinee_type);
            let air_pattern = self.lower_pattern(&expanded, resolved_enum);

            air_arms.push((air_pattern, body_result.air_ref));
        }

        // ADR-0051 Phase 5 + ADR-0052 Phase 4: exhaustiveness and
        // reachability via a single Maranget usefulness pass over the
        // AIR pattern matrix. Non-exhaustive matches surface nested
        // witnesses like `Some(None)`; unreachable arms fire one
        // `UnreachablePattern` warning each.
        let air_pattern_list: Vec<crate::inst::AirPattern> =
            air_arms.iter().map(|(p, _)| p.clone()).collect();
        let missing_witnesses = crate::sema::usefulness::exhaustiveness_witnesses(
            &air_pattern_list,
            scrutinee_type,
            self,
        );
        let missing: Vec<String> = missing_witnesses
            .iter()
            .map(|w| crate::sema::usefulness::render_witness(w, &self.type_pool))
            .collect();

        if !missing.is_empty() {
            return Err(CompileError::new(
                ErrorKind::NonExhaustiveMatch { missing },
                span,
            ));
        }

        // Exhaustive matches: check each arm's reachability against the
        // preceding arms. The legacy per-literal / per-variant trackers
        // retire here — Maranget subsumes them uniformly.
        let reachability =
            crate::sema::usefulness::arm_reachability(&air_pattern_list, scrutinee_type, self);
        for (i, (reachable, arm_pattern)) in
            reachability.iter().zip(air_pattern_list.iter()).enumerate()
        {
            if !*reachable {
                let pat_str = crate::sema::usefulness::render_witness(arm_pattern, &self.type_pool);
                ctx.warnings.push(
                    CompileWarning::new(
                        WarningKind::UnreachablePattern(pat_str),
                        arm_spans[i],
                    )
                    .with_note(
                        "this pattern will never be matched because earlier arms already cover every value it matches",
                    ),
                );
            }
        }

        let final_type = result_type.unwrap_or(Type::UNIT);

        // Encode match arms into extra array
        let arms_len = air_arms.len() as u32;
        let mut extra_data = Vec::new();
        for (pattern, body) in &air_arms {
            pattern.encode(*body, &mut extra_data);
        }
        let arms_start = air.add_extra(&extra_data);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Match {
                scrutinee: scrutinee_result.air_ref,
                arms_start,
                arms_len,
            },
            ty: final_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, final_type))
    }

    /// ADR-0051 Phase 4c: walk a Tuple / Struct / Ident match-arm pattern
    /// recursively and emit `FieldGet` + `StorageLive` + `Alloc` setup for
    /// every `RirPattern::Ident` leaf, registering each as a local in the
    /// arm scope. Non-binding leaves (`Wildcard`, `Int`, `Bool`) emit no
    /// setup — the cascading CFG dispatch inspects them directly.
    ///
    /// `scr_air_ref` is the AIR value of the (possibly projected) current
    /// focus; `scr_ty` is its type. Unknown struct lookups are treated as
    /// no-ops — the outer type-checking already surfaced those errors.
    pub(crate) fn emit_recursive_pattern_bindings(
        &self,
        air: &mut Air,
        scr_air_ref: AirRef,
        scr_ty: Type,
        pattern: &RirPattern,
        ctx: &mut AnalysisContext,
        out: &mut BindingEmission<'_>,
    ) {
        match pattern {
            RirPattern::Ident { name, is_mut, span } => {
                let slot = ctx.next_slot;
                ctx.next_slot += 1;
                out.storage_lives.push(air.add_inst(AirInst {
                    data: AirInstData::StorageLive { slot },
                    ty: scr_ty,
                    span: *span,
                }));
                out.allocs.push(air.add_inst(AirInst {
                    data: AirInstData::Alloc {
                        slot,
                        init: scr_air_ref,
                    },
                    ty: Type::UNIT,
                    span: *span,
                }));
                ctx.insert_local(
                    *name,
                    LocalVar {
                        slot,
                        ty: scr_ty,
                        is_mut: *is_mut,
                        span: *span,
                        allow_unused: false,
                    },
                );
            }
            RirPattern::Tuple { elems, .. } => {
                let Some(struct_id) = scr_ty.as_struct() else {
                    return;
                };
                let struct_def = self.type_pool.struct_def(struct_id);
                for (i, elem) in elems.iter().enumerate() {
                    let Some(field_ty) = struct_def.fields.get(i).map(|f| f.ty) else {
                        return;
                    };
                    let field_val = air.add_inst(AirInst {
                        data: AirInstData::FieldGet {
                            base: scr_air_ref,
                            struct_id,
                            field_index: i as u32,
                        },
                        ty: field_ty,
                        span: elem.span(),
                    });
                    self.emit_recursive_pattern_bindings(air, field_val, field_ty, elem, ctx, out);
                }
            }
            RirPattern::Struct { fields, .. } => {
                let Some(struct_id) = scr_ty.as_struct() else {
                    return;
                };
                let struct_def = self.type_pool.struct_def(struct_id);
                for rf in fields {
                    let field_name = self.interner.resolve(&rf.field_name);
                    let Some(idx) = struct_def
                        .fields
                        .iter()
                        .position(|sf| sf.name == field_name)
                    else {
                        continue;
                    };
                    let field_ty = struct_def.fields[idx].ty;
                    let field_val = air.add_inst(AirInst {
                        data: AirInstData::FieldGet {
                            base: scr_air_ref,
                            struct_id,
                            field_index: idx as u32,
                        },
                        ty: field_ty,
                        span: rf.pattern.span(),
                    });
                    self.emit_recursive_pattern_bindings(
                        air,
                        field_val,
                        field_ty,
                        &rf.pattern,
                        ctx,
                        out,
                    );
                }
            }
            RirPattern::DataVariant { bindings, .. } => {
                // ADR-0052: refutable nested variant sub-pattern. CFG's
                // cascading dispatch already verified this variant's
                // discriminant before entering the arm body, so extracting
                // the payload here is safe. Recurse into each binding's
                // slot (flat leaf or further nested).
                let Some((enum_id, variant_index)) = self.resolve_enum_from_pattern(pattern) else {
                    return;
                };
                let enum_def = self.type_pool.enum_def(enum_id);
                let variant_def = &enum_def.variants[variant_index as usize];
                for (field_index, binding) in bindings.iter().enumerate() {
                    let Some(field_ty) = variant_def.fields.get(field_index).copied() else {
                        return;
                    };
                    let field_val = air.add_inst(AirInst {
                        data: AirInstData::EnumPayloadGet {
                            base: scr_air_ref,
                            variant_index,
                            field_index: field_index as u32,
                        },
                        ty: field_ty,
                        span: pattern.span(),
                    });
                    self.emit_binding_setup(
                        air,
                        BoundField {
                            val: field_val,
                            ty: field_ty,
                            span: pattern.span(),
                        },
                        binding,
                        ctx,
                        out,
                    );
                }
            }
            RirPattern::StructVariant { field_bindings, .. } => {
                let Some((enum_id, variant_index)) = self.resolve_enum_from_pattern(pattern) else {
                    return;
                };
                let enum_def = self.type_pool.enum_def(enum_id);
                let variant_def = &enum_def.variants[variant_index as usize];
                let rest_marker = self.interner.get_or_intern_static("..");
                for fb in field_bindings {
                    if fb.field_name == rest_marker {
                        continue;
                    }
                    let Some(idx) = variant_def.find_field(self.interner.resolve(&fb.field_name))
                    else {
                        continue;
                    };
                    let field_ty = variant_def.fields[idx];
                    let field_val = air.add_inst(AirInst {
                        data: AirInstData::EnumPayloadGet {
                            base: scr_air_ref,
                            variant_index,
                            field_index: idx as u32,
                        },
                        ty: field_ty,
                        span: pattern.span(),
                    });
                    self.emit_binding_setup(
                        air,
                        BoundField {
                            val: field_val,
                            ty: field_ty,
                            span: pattern.span(),
                        },
                        &fb.binding,
                        ctx,
                        out,
                    );
                }
            }
            // Wildcard / Int / Bool / Path leaves bind no local.
            _ => {}
        }
    }

    /// ADR-0052 helper: install a single `RirPatternBinding` given the
    /// field value already extracted. Handles wildcard, named leaf, and
    /// nested `sub_pattern` cases uniformly with
    /// `emit_recursive_pattern_bindings`.
    fn emit_binding_setup(
        &self,
        air: &mut Air,
        field: BoundField,
        binding: &RirPatternBinding,
        ctx: &mut AnalysisContext,
        out: &mut BindingEmission<'_>,
    ) {
        let BoundField {
            val: field_val,
            ty: field_ty,
            span: pattern_span,
        } = field;
        if let Some(sub) = &binding.sub_pattern {
            self.emit_recursive_pattern_bindings(air, field_val, field_ty, sub, ctx, out);
            return;
        }
        if binding.is_wildcard {
            return;
        }
        let Some(name) = binding.name else {
            return;
        };
        let slot = ctx.next_slot;
        ctx.next_slot += 1;
        out.storage_lives.push(air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot },
            ty: field_ty,
            span: pattern_span,
        }));
        out.allocs.push(air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot,
                init: field_val,
            },
            ty: Type::UNIT,
            span: pattern_span,
        }));
        ctx.insert_local(
            name,
            LocalVar {
                slot,
                ty: field_ty,
                is_mut: binding.is_mut,
                span: pattern_span,
                allow_unused: false,
            },
        );
    }

    /// Try to resolve `(enum_id, variant_index)` for a Path / DataVariant /
    /// StructVariant pattern at lowering time. Used by `binding_to_pattern`
    /// when recursively lowering a nested sub-pattern whose outer
    /// validation pass already ran. Returns `None` for non-enum patterns
    /// or when the enum name is unknown (caller falls back to
    /// `AirPattern::Wildcard`).
    pub(crate) fn resolve_enum_from_pattern(
        &self,
        pattern: &RirPattern,
    ) -> Option<(crate::types::EnumId, u32)> {
        let (type_name, variant) = match pattern {
            RirPattern::Path {
                type_name, variant, ..
            }
            | RirPattern::DataVariant {
                type_name, variant, ..
            }
            | RirPattern::StructVariant {
                type_name, variant, ..
            } => (*type_name, *variant),
            _ => return None,
        };
        let enum_id = *self.enums.get(&type_name)?;
        let enum_def = self.type_pool.enum_def(enum_id);
        let variant_name = self.interner.resolve(&variant);
        let variant_index = enum_def.find_variant(variant_name)? as u32;
        Some((enum_id, variant_index))
    }

    /// Lower an RIR pattern to the recursive `AirPattern` shape
    /// (ADR-0051). `resolved_enum` carries `(enum_id, variant_index)`
    /// that the validation pass already computed for enum patterns;
    /// `None` means the pattern is not an enum arm. Nested sub-patterns
    /// in variant bindings recurse via `binding_to_pattern`.
    pub(crate) fn lower_pattern(
        &self,
        pattern: &RirPattern,
        resolved_enum: Option<(crate::types::EnumId, u32)>,
    ) -> AirPattern {
        match pattern {
            RirPattern::Wildcard(_) => AirPattern::Wildcard,
            RirPattern::Int(n, _) => AirPattern::Int(*n),
            RirPattern::Bool(b, _) => AirPattern::Bool(*b),
            RirPattern::Path { .. } => {
                let (enum_id, variant_index) =
                    resolved_enum.expect("resolved_enum must be set for enum patterns");
                AirPattern::EnumUnitVariant {
                    enum_id,
                    variant_index,
                }
            }
            RirPattern::DataVariant { bindings, .. } => {
                let (enum_id, variant_index) =
                    resolved_enum.expect("resolved_enum must be set for enum patterns");
                let enum_def = self.type_pool.enum_def(enum_id);
                let field_count = enum_def.variants[variant_index as usize].fields.len();
                let rest_marker = self.interner.get_or_intern_static("..");

                // Expand bindings to exactly `field_count` AirPatterns. A
                // trailing `..` (stored as a binding whose `name` is the rest
                // marker) is expanded to wildcard leaves covering the
                // unlisted fields (ADR-0049 §Phase 6 behaviour, preserved).
                let explicit: Vec<_> = bindings
                    .iter()
                    .filter(|b| b.name != Some(rest_marker))
                    .collect();
                let mut fields = Vec::with_capacity(field_count);
                for b in &explicit {
                    fields.push(binding_to_pattern(self, b));
                }
                while fields.len() < field_count {
                    fields.push(AirPattern::Wildcard);
                }

                AirPattern::EnumDataVariant {
                    enum_id,
                    variant_index,
                    fields,
                }
            }
            RirPattern::StructVariant { field_bindings, .. } => {
                let (enum_id, variant_index) =
                    resolved_enum.expect("resolved_enum must be set for enum patterns");
                let enum_def = self.type_pool.enum_def(enum_id);
                let variant_def = &enum_def.variants[variant_index as usize];
                let rest_marker = self.interner.get_or_intern_static("..");

                // Record explicit fields in declaration order; listed
                // positions carry the binding's pattern, every other
                // declared field defaults to `Wildcard` (covers both `..`
                // and field_bindings written out of order).
                let mut fields: Vec<(u32, AirPattern)> = Vec::new();
                for (field_idx, field_name) in variant_def.field_names.iter().enumerate() {
                    let matching = field_bindings.iter().find(|fb| {
                        fb.field_name != rest_marker
                            && self.interner.resolve(&fb.field_name) == field_name.as_str()
                    });
                    let pat = match matching {
                        Some(fb) => binding_to_pattern(self, &fb.binding),
                        None => AirPattern::Wildcard,
                    };
                    fields.push((field_idx as u32, pat));
                }

                AirPattern::EnumStructVariant {
                    enum_id,
                    variant_index,
                    fields,
                }
            }
            RirPattern::Ident { name, is_mut, .. } => AirPattern::Bind {
                name: *name,
                is_mut: *is_mut,
                inner: None,
            },
            RirPattern::Tuple { elems, .. } => {
                let air_elems: Vec<AirPattern> =
                    elems.iter().map(|e| self.lower_pattern(e, None)).collect();
                AirPattern::Tuple { elems: air_elems }
            }
            RirPattern::Struct {
                type_name, fields, ..
            } => {
                // Look up the struct by name to resolve declaration-order
                // field indices. Fall back to leaving fields in source order
                // if the lookup fails — sema validation will surface the
                // underlying type error.
                let struct_id = self.structs.get(type_name).copied();
                let field_indices: Vec<(u32, AirPattern)> = fields
                    .iter()
                    .filter_map(|f| {
                        let sid = struct_id?;
                        let def = self.type_pool.struct_def(sid);
                        let idx = def
                            .fields
                            .iter()
                            .position(|sf| sf.name == self.interner.resolve(&f.field_name))?;
                        Some((idx as u32, self.lower_pattern(&f.pattern, None)))
                    })
                    .collect();
                match struct_id {
                    Some(sid) => AirPattern::Struct {
                        struct_id: sid,
                        fields: field_indices,
                    },
                    None => AirPattern::Wildcard,
                }
            }
            // ADR-0079 Phase 3: should have been expanded earlier;
            // arriving here is an ICE.
            RirPattern::ComptimeUnrollArm { .. } => AirPattern::Wildcard,
        }
    }

    /// Analyze a return statement.
    fn analyze_return(
        &mut self,
        air: &mut Air,
        inner: Option<InstRef>,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inner_air_ref = if let Some(inner) = inner {
            // Explicit return with value
            let inner_result = self.analyze_inst(air, inner, ctx)?;
            let inner_ty = inner_result.ty;

            // ADR-0062: references are scope-bound — they cannot escape the
            // function in which they are constructed. Reject any attempt to
            // return a `Ref(T)` / `MutRef(T)` value.
            if inner_ty.is_any_ref() {
                return Err(CompileError::new(
                    ErrorKind::ReferenceEscapesFunction {
                        type_name: self.type_pool.format_type_name(inner_ty),
                    },
                    span,
                ));
            }

            // Type check: returned value must match function's return type.
            if !ctx.return_type.is_error()
                && !inner_ty.is_error()
                && !inner_ty.can_coerce_to(&ctx.return_type)
            {
                return Err(CompileError::type_mismatch(
                    ctx.return_type.name().to_string(),
                    inner_ty.name().to_string(),
                    span,
                ));
            }
            Some(inner_result.air_ref)
        } else {
            // `return;` without expression - only valid for unit-returning functions
            if ctx.return_type != Type::UNIT && !ctx.return_type.is_error() {
                return Err(CompileError::type_mismatch(
                    ctx.return_type.name().to_string(),
                    "()".to_string(),
                    span,
                ));
            }
            None
        };

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Ret(inner_air_ref),
            ty: Type::NEVER, // Return expressions have Never type
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::NEVER))
    }

    /// Analyze a block expression.
    fn analyze_block(
        &mut self,
        air: &mut Air,
        extra_start: u32,
        len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Get the instruction refs from extra data
        let inst_refs = self.rir.get_extra(extra_start, len);

        // Push a new scope for this block.
        ctx.push_scope();

        // Process all instructions in the block
        let mut statements = Vec::new();
        let mut last_result: Option<AnalysisResult> = None;
        let num_insts = inst_refs.len();
        for (i, &raw_ref) in inst_refs.iter().enumerate() {
            let inst_ref = InstRef::from_raw(raw_ref);
            let is_last = i == num_insts - 1;
            let result = self.analyze_inst(air, inst_ref, ctx)?;

            if is_last {
                last_result = Some(result);
            } else {
                statements.push(result.air_ref);
            }
        }

        // Check for unconsumed linear values before popping scope
        self.check_unconsumed_linear_values(ctx)?;

        // Check for unused variables before popping scope
        self.check_unused_locals_in_current_scope(ctx);

        // Pop scope to remove block-scoped variables.
        ctx.pop_scope();

        // Handle empty blocks - they evaluate to Unit
        let last = match last_result {
            Some(result) => result,
            None => {
                // Empty block: create a UnitConst
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty: Type::UNIT,
                    span,
                });
                AnalysisResult::new(air_ref, Type::UNIT)
            }
        };

        // Only create a Block instruction if there are statements;
        // otherwise just return the value directly (optimization)
        if statements.is_empty() {
            Ok(last)
        } else {
            let ty = last.ty;
            let stmt_u32s: Vec<u32> = statements.iter().map(|r| r.as_u32()).collect();
            let stmts_start = air.add_extra(&stmt_u32s);
            let stmts_len = statements.len() as u32;
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Block {
                    stmts_start,
                    stmts_len,
                    value: last.air_ref,
                },
                ty,
                span,
            });
            Ok(AnalysisResult::new(air_ref, ty))
        }
    }

    // ========================================================================
    // Variable operations: Alloc, VarRef, ParamRef, Assign
    // ========================================================================

    /// Analyze a variable operation instruction.
    ///
    /// Handles: Alloc, VarRef, ParamRef, Assign
    pub(crate) fn analyze_variable_ops(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::Alloc { .. } => self.analyze_alloc(air, inst_ref, ctx),

            InstData::StructDestructure { .. } => {
                self.analyze_struct_destructure(air, inst_ref, ctx)
            }

            InstData::VarRef { name } => self.analyze_var_ref(air, inst_ref, *name, inst.span, ctx),

            InstData::ParamRef { index: _, name } => {
                self.analyze_param_ref(air, *name, inst.span, ctx)
            }

            InstData::Assign { name, value } => {
                self.analyze_assign(air, *name, *value, inst.span, ctx)
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_variable_ops called with non-variable instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }

    /// Analyze a local variable allocation.
    fn analyze_alloc(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);
        let (directives_start, directives_len, name, is_mut, init, span) = match inst.data {
            InstData::Alloc {
                directives_start,
                directives_len,
                name,
                is_mut,
                ty: _,
                init,
            } => (
                directives_start,
                directives_len,
                name,
                is_mut,
                init,
                inst.span,
            ),
            _ => unreachable!("analyze_alloc called with non-Alloc instruction"),
        };

        // ADR-0079 Phase 2b: detect `let mut h = @uninit(T)` and
        // `let mut h = @variant_uninit(T, tag)` patterns. Instead of
        // analyzing the init normally, we register `h` as an
        // in-progress construction handle and skip allocation. Later
        // `@field_set(h, ...)` calls record field writes; `@finalize(h)`
        // emits the actual `StructInit`.
        if let Some(name) = name
            && let Some(handle) = self.try_capture_uninit_init(init, span, ctx)?
        {
            // Stash the handle keyed by binding name; subsequent
            // uses (`@field_set`, `@finalize`, errors-on-escape)
            // resolve it from `ctx.uninit_handles`.
            ctx.uninit_handles.insert(name, handle);
            // Also record an empty entry in `ctx.locals` so name
            // resolution finds the binding (we need this so a stray
            // VarRef produces a "uninit handle escaped" diagnostic
            // rather than "undefined variable"). Use the special
            // sentinel slot `u32::MAX` to mark uninit-handle locals.
            ctx.insert_local(
                name,
                LocalVar {
                    slot: u32::MAX,
                    ty: Type::UNIT, // sentinel; never read for codegen
                    is_mut,
                    span,
                    allow_unused: false,
                },
            );
            // Producer is the unit-valued `Alloc`; emit it as a
            // unit so the surrounding statement composes normally.
            let unit_ref = air.add_inst(AirInst {
                data: AirInstData::UnitConst,
                ty: Type::UNIT,
                span,
            });
            return Ok(AnalysisResult::new(unit_ref, Type::UNIT));
        }

        // Analyze the initializer
        let init_result = self.analyze_inst(air, init, ctx)?;
        let var_type = init_result.ty;

        // If name is None, this is a wildcard pattern `_` that discards the value
        let Some(name) = name else {
            return Ok(AnalysisResult::new(init_result.air_ref, Type::UNIT));
        };

        // Special case: comptime type variables
        // When a variable is assigned a comptime type value (e.g., `let P = make_type()`),
        // we store the type in comptime_type_vars instead of creating a runtime variable.
        // This allows the variable to be used as a type annotation later (e.g., `let p: P = ...`).
        if var_type == Type::COMPTIME_TYPE {
            // Extract the type value from the TypeConst instruction
            let inst = air.get(init_result.air_ref);
            if let AirInstData::TypeConst(ty) = &inst.data {
                ctx.comptime_type_vars.insert(name, *ty);
                // Return Unit - no runtime code is generated for comptime type bindings
                let nop_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty: Type::UNIT,
                    span,
                });
                return Ok(AnalysisResult::new(nop_ref, Type::UNIT));
            }
            // If it's not a TypeConst, fall through to error (can't store types at runtime)
            let name_str = self.interner.resolve(&name);
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "cannot store type value in variable '{}' at runtime; \
                         type values only exist at compile time",
                        name_str
                    ),
                },
                span,
            ));
        }

        // Check if @allow(unused_variable) directive is present
        let directives = self.rir.get_directives(directives_start, directives_len);
        let allow_unused = self.has_allow_directive(&directives, "unused_variable");

        // Allocate slots
        let slot = ctx.next_slot;
        let num_slots = self.abi_slot_count(var_type);
        ctx.next_slot += num_slots;

        // Register the variable
        ctx.insert_local(
            name,
            LocalVar {
                slot,
                ty: var_type,
                is_mut,
                span,
                allow_unused,
            },
        );

        // Emit StorageLive to mark the slot as live
        let storage_live_ref = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot },
            ty: var_type,
            span,
        });

        // Emit the alloc instruction
        let alloc_ref = air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot,
                init: init_result.air_ref,
            },
            ty: Type::UNIT,
            span,
        });

        // Return a block containing both StorageLive and Alloc
        let stmts_start = air.add_extra(&[storage_live_ref.as_u32()]);
        let block_ref = air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start,
                stmts_len: 1,
                value: alloc_ref,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(block_ref, Type::UNIT))
    }

    /// ADR-0079 Phase 2b: peek at a let-binding's init RIR; if it's
    /// `@uninit(T)` or `@variant_uninit(T, tag)`, return a fresh
    /// `UninitHandle` describing the pending construction. Returns
    /// `Ok(None)` for any other init shape — the caller falls
    /// through to normal alloc analysis.
    fn try_capture_uninit_init(
        &mut self,
        init: InstRef,
        _span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Option<crate::sema::context::UninitHandle>> {
        use crate::sema::context::{UninitHandle, UninitTarget};

        let init_inst = self.rir.get(init);
        match init_inst.data {
            // `@uninit(T)` — TypeIntrinsic with name="uninit".
            InstData::TypeIntrinsic { name, type_arg } => {
                let intrinsic_name = self.interner.resolve(&name);
                if intrinsic_name != "uninit" {
                    return Ok(None);
                }
                let ty = self.resolve_type(type_arg, init_inst.span)?;
                let struct_id = ty.as_struct().ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "@uninit(T) requires T to be a struct type, got {}",
                                ty.name()
                            ),
                        },
                        init_inst.span,
                    )
                })?;
                Ok(Some(UninitHandle {
                    target: UninitTarget::Struct(struct_id),
                    fields: HashMap::default(),
                }))
            }
            // `@variant_uninit(T, tag)` — Intrinsic with name="variant_uninit",
            // expression args. First arg is the type (parsed as
            // IntrinsicArg::Type), second arg is a comptime variant
            // value.
            InstData::Intrinsic {
                name,
                args_start,
                args_len,
            } => {
                let intrinsic_name = self.interner.resolve(&name);
                if intrinsic_name != "variant_uninit" {
                    return Ok(None);
                }
                if args_len != 2 {
                    return Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "@variant_uninit(T, tag) takes 2 args, got {}",
                                args_len
                            ),
                        },
                        init_inst.span,
                    ));
                }
                let arg_refs = self.rir.get_inst_refs(args_start, args_len);
                // First arg: type expression. Second arg: comptime tag.
                let ty_ref = arg_refs[0];
                let tag_ref = arg_refs[1];
                let ty = self.resolve_intrinsic_type_arg(ty_ref, ctx)?;
                let enum_id = ty.as_enum().ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "@variant_uninit(T, _) requires T to be an enum type, got {}",
                                ty.name()
                            ),
                        },
                        init_inst.span,
                    )
                })?;
                let variant_idx =
                    self.evaluate_variant_tag_arg(tag_ref, enum_id, ctx, init_inst.span)?;
                Ok(Some(UninitHandle {
                    target: UninitTarget::EnumVariant {
                        enum_id,
                        variant_idx,
                    },
                    fields: HashMap::default(),
                }))
            }
            _ => Ok(None),
        }
    }

    /// Resolve an intrinsic's first type-position argument. Most type
    /// intrinsics route through `TypeIntrinsic`, but `@variant_uninit`
    /// is a mixed-arg intrinsic (`Intrinsic`) so we have to peek at
    /// the first arg and handle both `TypeConst`-like literals and
    /// already-resolved comptime type values.
    fn resolve_intrinsic_type_arg(
        &mut self,
        type_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Type> {
        use crate::sema::context::ConstValue;
        let type_inst = self.rir.get(type_ref);
        let span = type_inst.span;
        if let InstData::TypeConst { type_name } = type_inst.data {
            return self.resolve_type(type_name, span);
        }
        // Fall back to comptime evaluation; expects a `ConstValue::Type`.
        let val = self.evaluate_comptime_block(type_ref, ctx, span)?;
        match val {
            ConstValue::Type(t) => Ok(t),
            other => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!("expected a type argument, got comptime value {:?}", other),
                },
                span,
            )),
        }
    }

    /// Evaluate a comptime variant-tag expression and resolve to a
    /// variant index of the given enum. Used by `@variant_uninit`
    /// and `@variant_field` to convert a comptime `VariantInfo` /
    /// enum-variant value to a concrete index.
    fn evaluate_variant_tag_arg(
        &mut self,
        tag_ref: InstRef,
        enum_id: crate::types::EnumId,
        ctx: &mut AnalysisContext,
        span: Span,
    ) -> CompileResult<u32> {
        use crate::sema::context::{ComptimeHeapItem, ConstValue};
        // ADR-0079 Phase 3: callers can come from comptime_unroll for
        // bodies where the surrounding heap holds the loop variable
        // (e.g. a `VariantInfo` struct whose `name` field is the tag
        // string). `evaluate_comptime_block` would clear that heap
        // and invalidate the value, so use the heap-preserving
        // evaluator.
        let prev_steps = self.comptime_steps_used;
        self.comptime_steps_used = 0;
        let mut locals = ctx.comptime_value_vars.clone();
        let val = self.evaluate_comptime_inst(tag_ref, &mut locals, ctx, span)?;
        self.comptime_steps_used = prev_steps;
        match val {
            ConstValue::EnumVariant {
                enum_id: e,
                variant_idx,
            } if e == enum_id => Ok(variant_idx),
            ConstValue::EnumData {
                enum_id: e,
                variant_idx,
                ..
            } if e == enum_id => Ok(variant_idx),
            // Accept a comptime variant *name* string (e.g.
            // `v.name` from `@type_info(Self).variants`); look it
            // up in the enum's variant list.
            ConstValue::ComptimeStr(idx) => {
                let name =
                    match &self.comptime_heap[idx as usize] {
                        ComptimeHeapItem::String(s) => s.clone(),
                        _ => return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason:
                                    "expected a comptime variant tag (variant value or name string)"
                                        .into(),
                            },
                            span,
                        )),
                    };
                let enum_def = self.type_pool.enum_def(enum_id);
                enum_def
                    .variants
                    .iter()
                    .position(|v| v.name == name)
                    .map(|i| i as u32)
                    .ok_or_else(|| {
                        CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: format!(
                                    "no variant named `{}` on enum `{}`",
                                    name, enum_def.name
                                ),
                            },
                            span,
                        )
                    })
            }
            _ => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "expected a comptime variant tag of the matching enum".into(),
                },
                span,
            )),
        }
    }

    /// ADR-0079 Phase 2b: analyze `@field_set(handle, name, value)`.
    /// Looks up the named binding's uninit handle in the analysis
    /// context, records the field's analyzed value, and returns
    /// unit. Errors if `handle` isn't a known uninit handle, the
    /// field doesn't exist on the target type, or the field is
    /// already written.
    pub(crate) fn analyze_field_set_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 3 {
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!("@field_set takes 3 args, got {}", args.len()),
                },
                span,
            ));
        }
        let handle_ref = args[0].value;
        let name_ref = args[1].value;
        let value_ref = args[2].value;

        // The first arg must be a VarRef to an uninit-handle binding.
        let handle_inst = self.rir.get(handle_ref);
        let handle_name = match handle_inst.data {
            InstData::VarRef { name } => name,
            _ => {
                return Err(CompileError::new(
                    ErrorKind::ComptimeEvaluationFailed {
                        reason: "@field_set's first argument must be an uninit-handle binding"
                            .into(),
                    },
                    span,
                ));
            }
        };
        if !ctx.uninit_handles.contains_key(&handle_name) {
            let name_str = self.interner.resolve(&handle_name).to_string();
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "`{}` is not an uninit handle (must be bound by @uninit or @variant_uninit)",
                        name_str
                    ),
                },
                span,
            ));
        }

        // Evaluate the field name at comptime.
        let field_name = self.evaluate_comptime_str(name_ref, ctx, span)?;
        let field_spur = self.interner.get_or_intern(&field_name);

        // Determine the expected field type from the handle's target.
        let handle_target = ctx.uninit_handles[&handle_name].target;
        let expected_ty = self.uninit_handle_field_type(handle_target, &field_name, span)?;

        // Analyze the value with the expected type (so int literals
        // coerce correctly).
        let value_result = self.analyze_inst(air, value_ref, ctx)?;
        if value_result.ty != expected_ty
            && !value_result.ty.is_never()
            && !value_result.ty.is_error()
        {
            return Err(CompileError::type_mismatch(
                expected_ty.name().to_string(),
                value_result.ty.name().to_string(),
                span,
            ));
        }

        // Record the write. Reject duplicates.
        let handle = ctx.uninit_handles.get_mut(&handle_name).unwrap();
        if handle.fields.contains_key(&field_spur) {
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "@field_set wrote field `{}` twice on the same uninit handle",
                        field_name
                    ),
                },
                span,
            ));
        }
        handle.fields.insert(field_spur, value_result.air_ref);

        // @field_set yields unit.
        let unit_ref = air.add_inst(AirInst {
            data: AirInstData::UnitConst,
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(unit_ref, Type::UNIT))
    }

    /// ADR-0079 Phase 2b: analyze `@finalize(handle)`. Consumes the
    /// uninit handle, verifies all fields written, and emits a
    /// regular `StructInit` (or enum-variant construction).
    pub(crate) fn analyze_finalize_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!("@finalize takes 1 arg, got {}", args.len()),
                },
                span,
            ));
        }
        let handle_ref = args[0].value;
        let handle_inst = self.rir.get(handle_ref);
        let handle_name = match handle_inst.data {
            InstData::VarRef { name } => name,
            _ => {
                return Err(CompileError::new(
                    ErrorKind::ComptimeEvaluationFailed {
                        reason: "@finalize's argument must be an uninit-handle binding".into(),
                    },
                    span,
                ));
            }
        };
        let handle = ctx.uninit_handles.remove(&handle_name).ok_or_else(|| {
            let name_str = self.interner.resolve(&handle_name).to_string();
            CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "`{}` is not an uninit handle (or has already been @finalize'd)",
                        name_str
                    ),
                },
                span,
            )
        })?;
        // Also remove the local sentinel so subsequent stray uses
        // produce a clean "undefined" diagnostic.
        ctx.locals.remove(&handle_name);

        match handle.target {
            crate::sema::context::UninitTarget::Struct(struct_id) => {
                self.emit_uninit_struct_finalize(air, struct_id, &handle.fields, span)
            }
            crate::sema::context::UninitTarget::EnumVariant {
                enum_id,
                variant_idx,
            } => self.emit_uninit_variant_finalize(air, enum_id, variant_idx, &handle.fields, span),
        }
    }

    /// ADR-0079 Phase 3: analyze `@variant_field(self, comptime tag, name)`.
    /// Reads the named field of variant `tag` from `self`. The `tag` is
    /// a comptime variant value (e.g. an element of
    /// `@type_info(Self).variants`), `name` is a comptime string. The
    /// surrounding context is responsible for ensuring `self` is of
    /// variant `tag` — typically that's a `comptime_unroll for`-driven
    /// match-arm, but a stray call still type-checks against the
    /// declared field type.
    pub(crate) fn analyze_variant_field_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 3 {
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "@variant_field takes 3 args (self, tag, name), got {}",
                        args.len()
                    ),
                },
                span,
            ));
        }

        // Analyze the receiver to get its type.
        let recv = self.analyze_inst(air, args[0].value, ctx)?;
        let recv_ty = recv.ty;
        let enum_ty = match recv_ty.kind() {
            crate::types::TypeKind::Ref(id) => self.type_pool.ref_def(id),
            crate::types::TypeKind::MutRef(id) => self.type_pool.mut_ref_def(id),
            _ => recv_ty,
        };
        let enum_id = enum_ty.as_enum().ok_or_else(|| {
            CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "@variant_field expects an enum receiver, got {}",
                        recv_ty.name()
                    ),
                },
                span,
            )
        })?;

        // Resolve the tag (a comptime variant value) to a variant index.
        let variant_idx = self.evaluate_variant_tag_arg(args[1].value, enum_id, ctx, span)?;

        // Resolve the field name.
        let field_name = self.evaluate_comptime_str(args[2].value, ctx, span)?;

        // Look up the field's type and index in the variant.
        let enum_def = self.type_pool.enum_def(enum_id);
        let variant = enum_def.variants.get(variant_idx as usize).ok_or_else(|| {
            CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "variant index {} out of range for enum `{}`",
                        variant_idx, enum_def.name
                    ),
                },
                span,
            )
        })?;
        let field_idx = variant_field_index(variant, &field_name).ok_or_else(|| {
            CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "variant `{}::{}` has no field named `{}`",
                        enum_def.name, variant.name, field_name
                    ),
                },
                span,
            )
        })? as u32;
        let field_ty = variant.fields[field_idx as usize];

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::EnumPayloadGet {
                base: recv.air_ref,
                variant_index: variant_idx,
                field_index: field_idx,
            },
            ty: field_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, field_ty))
    }

    /// Build an `AirInstData::StructInit` from a fully-written
    /// uninit handle's field map.
    fn emit_uninit_struct_finalize(
        &mut self,
        air: &mut Air,
        struct_id: crate::types::StructId,
        fields_written: &HashMap<Spur, AirRef>,
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        let struct_def = self.type_pool.struct_def(struct_id);
        let struct_name = struct_def.name.clone();

        // Verify completeness.
        let mut missing = Vec::new();
        for f in &struct_def.fields {
            let f_spur = self.interner.get_or_intern(&f.name);
            if !fields_written.contains_key(&f_spur) {
                missing.push(f.name.clone());
            }
        }
        if !missing.is_empty() {
            return Err(CompileError::new(
                ErrorKind::MissingFields(Box::new(MissingFieldsError {
                    struct_name,
                    missing_fields: missing,
                })),
                span,
            ));
        }

        // Reject extra fields.
        let known_names: HashSet<&str> =
            struct_def.fields.iter().map(|f| f.name.as_str()).collect();
        for &spur in fields_written.keys() {
            let name_str = self.interner.resolve(&spur);
            if !known_names.contains(name_str) {
                return Err(CompileError::new(
                    ErrorKind::UnknownField {
                        struct_name: struct_def.name.clone(),
                        field_name: name_str.to_string(),
                    },
                    span,
                ));
            }
        }

        // Emit StructInit in declared field order.
        let mut field_air_refs: Vec<u32> = Vec::with_capacity(struct_def.fields.len());
        let mut source_order: Vec<u32> = Vec::with_capacity(struct_def.fields.len());
        for (idx, f) in struct_def.fields.iter().enumerate() {
            let f_spur = self.interner.get_or_intern(&f.name);
            let value_ref = fields_written[&f_spur];
            field_air_refs.push(value_ref.as_u32());
            source_order.push(idx as u32);
        }
        let fields_start = air.add_extra(&field_air_refs);
        let source_order_start = air.add_extra(&source_order);
        let struct_ty = Type::new_struct(struct_id);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::StructInit {
                struct_id,
                fields_start,
                fields_len: struct_def.fields.len() as u32,
                source_order_start,
            },
            ty: struct_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, struct_ty))
    }

    /// Build a variant-construction AIR from a fully-written
    /// uninit-variant handle's field map.
    fn emit_uninit_variant_finalize(
        &mut self,
        air: &mut Air,
        enum_id: crate::types::EnumId,
        variant_idx: u32,
        fields_written: &HashMap<Spur, AirRef>,
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        let result_ty = Type::new_enum(enum_id);
        let enum_def = self.type_pool.enum_def(enum_id);
        let variant = enum_def.variants.get(variant_idx as usize).ok_or_else(|| {
            CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "variant index {} out of range for enum `{}`",
                        variant_idx, enum_def.name
                    ),
                },
                span,
            )
        })?;

        // Unit variant: no payload, emit a plain EnumVariant.
        if variant.fields.is_empty() {
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::EnumVariant {
                    enum_id,
                    variant_index: variant_idx,
                },
                ty: result_ty,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, result_ty));
        }

        // Data/struct variant: verify completeness, then emit
        // EnumCreate in declared field order. For tuple variants the
        // fields are addressed positionally as `"0"`, `"1"`, etc.
        let n_fields = variant.fields.len();
        let mut missing = Vec::new();
        for i in 0..n_fields {
            let fname = variant_field_name(variant, i);
            let f_spur = self.interner.get_or_intern(&fname);
            if !fields_written.contains_key(&f_spur) {
                missing.push(fname);
            }
        }
        if !missing.is_empty() {
            return Err(CompileError::new(
                ErrorKind::MissingFields(Box::new(MissingFieldsError {
                    struct_name: format!("{}::{}", enum_def.name, variant.name),
                    missing_fields: missing,
                })),
                span,
            ));
        }
        let known: HashSet<String> = (0..n_fields)
            .map(|i| variant_field_name(variant, i))
            .collect();
        for &spur in fields_written.keys() {
            let n = self.interner.resolve(&spur).to_string();
            if !known.contains(&n) {
                return Err(CompileError::new(
                    ErrorKind::UnknownField {
                        struct_name: format!("{}::{}", enum_def.name, variant.name),
                        field_name: n,
                    },
                    span,
                ));
            }
        }
        let mut field_air_refs: Vec<u32> = Vec::with_capacity(n_fields);
        for i in 0..n_fields {
            let fname = variant_field_name(variant, i);
            let f_spur = self.interner.get_or_intern(&fname);
            field_air_refs.push(fields_written[&f_spur].as_u32());
        }
        let fields_start = air.add_extra(&field_air_refs);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::EnumCreate {
                enum_id,
                variant_index: variant_idx,
                fields_start,
                fields_len: n_fields as u32,
            },
            ty: result_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_ty))
    }

    /// Look up the expected type of a named field on an uninit
    /// handle's target (struct or enum variant).
    fn uninit_handle_field_type(
        &self,
        target: crate::sema::context::UninitTarget,
        field_name: &str,
        span: Span,
    ) -> CompileResult<Type> {
        use crate::sema::context::UninitTarget;
        match target {
            UninitTarget::Struct(struct_id) => {
                let struct_def = self.type_pool.struct_def(struct_id);
                struct_def
                    .fields
                    .iter()
                    .find(|f| f.name == field_name)
                    .map(|f| f.ty)
                    .ok_or_else(|| {
                        CompileError::new(
                            ErrorKind::UnknownField {
                                struct_name: struct_def.name.clone(),
                                field_name: field_name.to_string(),
                            },
                            span,
                        )
                    })
            }
            UninitTarget::EnumVariant {
                enum_id,
                variant_idx,
            } => {
                let enum_def = self.type_pool.enum_def(enum_id);
                let variant = enum_def.variants.get(variant_idx as usize).ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "variant index {} out of range for enum `{}`",
                                variant_idx, enum_def.name
                            ),
                        },
                        span,
                    )
                })?;
                let idx = variant_field_index(variant, field_name).ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "variant `{}::{}` has no field named `{}`",
                                enum_def.name, variant.name, field_name
                            ),
                        },
                        span,
                    )
                })?;
                Ok(variant.fields[idx])
            }
        }
    }

    /// Evaluate a comptime expression to a string. Used by
    /// `@field_set(handle, name, value)` to resolve the field name.
    fn evaluate_comptime_str(
        &mut self,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
        span: Span,
    ) -> CompileResult<String> {
        use crate::sema::context::{ComptimeHeapItem, ConstValue};
        // ADR-0079 Phase 2b: callers reach here from inside
        // `comptime_unroll for` bodies where the surrounding heap holds
        // the loop variable's struct value. `evaluate_comptime_block`
        // would clear the heap and invalidate that handle, so we use
        // the heap-preserving evaluator instead.
        let prev_steps = self.comptime_steps_used;
        self.comptime_steps_used = 0;
        let mut locals = ctx.comptime_value_vars.clone();
        let result = self.evaluate_comptime_inst(inst_ref, &mut locals, ctx, span)?;
        self.comptime_steps_used = prev_steps;
        match result {
            ConstValue::ComptimeStr(idx) => match &self.comptime_heap[idx as usize] {
                ComptimeHeapItem::String(s) => Ok(s.clone()),
                _ => Err(CompileError::new(
                    ErrorKind::ComptimeEvaluationFailed {
                        reason: "expected a comptime string, got non-string heap value".into(),
                    },
                    span,
                )),
            },
            other => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!("expected a comptime string, got {:?}", other),
                },
                span,
            )),
        }
    }

    /// Analyze a struct destructuring pattern.
    fn analyze_struct_destructure(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);
        let span = inst.span;

        let (type_name, fields_start, fields_len, init) = match inst.data {
            InstData::StructDestructure {
                type_name,
                fields_start,
                fields_len,
                init,
            } => (type_name, fields_start, fields_len, init),
            _ => unreachable!(),
        };

        // Analyze the initializer expression
        let init_result = self.analyze_inst(air, init, ctx)?;
        let init_type = init_result.ty;

        // Resolve the struct type
        let type_name_str = self.interner.resolve(&type_name).to_string();
        // Tuple destructure sentinel: astgen emits "__tuple__" as the type_name
        // when desugaring `let (a, b, ...) = expr;` (ADR-0048). The actual struct
        // type comes from inference on `init`.
        let is_tuple_destructure = type_name_str == "__tuple__";

        let struct_id = init_type.as_struct().ok_or_else(|| {
            CompileError::type_mismatch(
                if is_tuple_destructure {
                    "tuple".to_string()
                } else {
                    type_name_str.clone()
                },
                init_type.name().to_string(),
                span,
            )
        })?;

        let struct_def = self.type_pool.struct_def(struct_id);

        // Validate the type name matches (skipped for tuple destructure — the
        // sentinel "__tuple__" doesn't correspond to a real struct name).
        //
        // The pattern's `type_name` may be a local alias of an anonymous
        // struct (ADR-0029 / ADR-0039 workflow): `let PairI32 = Pair(i32);
        // let PairI32 { first, second } = p;`. In that case the comptime
        // type variable resolves to the same StructId as `init_type`, and a
        // plain name comparison would spuriously reject the destructure as
        // `PairI32 vs __anon_struct_N`. Resolve the pattern's type name
        // through `comptime_type_vars` first; fall back to the name
        // comparison otherwise (ADR-0049 Phase 7).
        if !is_tuple_destructure {
            let resolved_struct_id = ctx
                .comptime_type_vars
                .get(&type_name)
                .and_then(|ty| match ty.kind() {
                    TypeKind::Struct(id) => Some(id),
                    _ => None,
                });
            let names_match = match resolved_struct_id {
                Some(alias_id) => alias_id == struct_id,
                None => struct_def.name == type_name_str,
            };
            if !names_match {
                return Err(CompileError::type_mismatch(
                    type_name_str,
                    struct_def.name.clone(),
                    span,
                ));
            }
        }

        // Get the destructure fields from the RIR.
        // Rest-pattern marker: astgen emits a synthetic `..` field when the
        // user writes `Point { x, .. }` (ADR-0049 Phase 6). We strip it here
        // and set `has_rest`, which disables the "all fields required" rule
        // below and causes every unlisted field to be treated as wildcard-
        // dropped.
        //
        // Suffix markers: `let (a, .., b)` on an N-tuple emits `..end_N-1-i`
        // for each suffix position. Resolve these to concrete numeric field
        // names once the struct arity is known.
        let all_rir_fields = self.rir.get_destructure_fields(fields_start, fields_len);
        let struct_arity = struct_def.fields.len();
        let (rir_fields, has_rest) = {
            let mut filtered = Vec::with_capacity(all_rir_fields.len());
            let mut has_rest = false;
            let mut prefix_count: usize = 0;
            let mut suffix_count: usize = 0;
            for mut f in all_rir_fields {
                let name_str = self.interner.resolve(&f.field_name).to_string();
                if name_str == ".." {
                    has_rest = true;
                    continue;
                }
                if let Some(rest) = name_str.strip_prefix("..end_") {
                    let from_end: usize = rest.parse().expect(
                        "astgen emits well-formed ..end_N markers; parser rejects user `..end_N` field names",
                    );
                    if from_end >= struct_arity {
                        return Err(CompileError::type_mismatch(
                            format!("tuple of arity at least {}", from_end + 1),
                            init_type.name().to_string(),
                            span,
                        ));
                    }
                    suffix_count += 1;
                    let idx = struct_arity - 1 - from_end;
                    f.field_name = self.interner.get_or_intern(idx.to_string());
                    filtered.push(f);
                } else {
                    prefix_count += 1;
                    filtered.push(f);
                }
            }
            // Prefix + suffix must fit within the tuple: a 2-tuple
            // matched against `(a, b, .., c, d)` would overlap prefix
            // and suffix positions, which is nonsensical.
            if has_rest && prefix_count + suffix_count > struct_arity {
                return Err(CompileError::type_mismatch(
                    format!("tuple of arity at least {}", prefix_count + suffix_count),
                    init_type.name().to_string(),
                    span,
                ));
            }
            (filtered, has_rest)
        };

        // Validate: no duplicate fields
        let mut seen_fields = rustc_hash::FxHashSet::default();
        for field in &rir_fields {
            let field_name = self.interner.resolve(&field.field_name).to_string();
            if !seen_fields.insert(field_name.clone()) {
                return Err(CompileError::new(
                    ErrorKind::DuplicateField {
                        struct_name: type_name_str.clone(),
                        field_name,
                    },
                    span,
                ));
            }
        }

        // Display name for error messages. For tuple destructures, render in
        // tuple syntax: `(i32, bool)` instead of the synthetic `__anon_struct_N`.
        let display_name = if is_tuple_destructure {
            let pool = &self.type_pool;
            struct_def
                .tuple_display_name(|ty| ty.safe_name_with_pool(Some(pool)))
                .unwrap_or_else(|| struct_def.name.clone())
        } else {
            type_name_str.clone()
        };

        // Validate: all struct fields are mentioned (waived when a `..` rest
        // pattern is present — missing fields are wildcard-bound below).
        let struct_field_names: Vec<String> =
            struct_def.fields.iter().map(|f| f.name.clone()).collect();
        if !has_rest {
            for struct_field_name in &struct_field_names {
                if !seen_fields.contains(struct_field_name) {
                    return Err(CompileError::new(
                        ErrorKind::MissingFieldInDestructure {
                            struct_name: display_name.clone(),
                            field: struct_field_name.clone(),
                        },
                        span,
                    ));
                }
            }
        }

        // Validate: no unknown fields
        for field in &rir_fields {
            let field_name = self.interner.resolve(&field.field_name).to_string();
            if struct_def.find_field(&field_name).is_none() {
                return Err(CompileError::new(
                    ErrorKind::UnknownField {
                        struct_name: display_name.clone(),
                        field_name,
                    },
                    span,
                ));
            }
        }

        // When `..` is present, synthesize wildcard `RirDestructureField`s for
        // every struct field not explicitly mentioned. The subsequent
        // alloc/drop loop treats them like any other wildcard binding, so
        // non-copy fields get dropped at scope exit.
        let rir_fields: Vec<RirDestructureField> = if has_rest {
            let rest_marker = self.interner.get_or_intern_static("..");
            let mut expanded = rir_fields;
            for struct_field_name in &struct_field_names {
                if !seen_fields.contains(struct_field_name) {
                    let field_name_spur = self.interner.get_or_intern(struct_field_name);
                    debug_assert!(field_name_spur != rest_marker);
                    expanded.push(RirDestructureField {
                        field_name: field_name_spur,
                        binding_name: None,
                        is_wildcard: true,
                        is_mut: false,
                    });
                }
            }
            expanded
        } else {
            rir_fields
        };

        // Emit AIR: store init into a temp, then extract each field into its own slot.
        // The temp struct slot is NOT registered with StorageLive —
        // field ownership is transferred to individual bindings.
        //
        // Structure: Block { stmts: [StorageLive...], value: inner_block }
        // The outer block is a "StorageLive wrapper" so no scope is pushed.
        // The inner block contains the allocs (no StorageLive, so its scope is empty).

        let temp_slot = ctx.next_slot;
        ctx.next_slot += 1;
        let temp_alloc = air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot: temp_slot,
                init: init_result.air_ref,
            },
            ty: Type::UNIT,
            span,
        });

        let mut storage_lives = Vec::new();
        let mut allocs = vec![temp_alloc];

        for field in &rir_fields {
            let field_name = self.interner.resolve(&field.field_name).to_string();
            let (field_index, struct_field) = struct_def.find_field(&field_name).unwrap();
            let field_type = struct_field.ty;

            // Load the struct from temp
            let temp_load = air.add_inst(AirInst {
                data: AirInstData::Load { slot: temp_slot },
                ty: init_type,
                span,
            });

            // Read the field
            let field_get = air.add_inst(AirInst {
                data: AirInstData::FieldGet {
                    base: temp_load,
                    struct_id,
                    field_index: field_index as u32,
                },
                ty: field_type,
                span,
            });

            // Allocate a slot for this field binding
            let field_slot = ctx.next_slot;
            ctx.next_slot += 1;

            let storage_live = air.add_inst(AirInst {
                data: AirInstData::StorageLive { slot: field_slot },
                ty: field_type,
                span,
            });
            storage_lives.push(storage_live);

            let field_alloc = air.add_inst(AirInst {
                data: AirInstData::Alloc {
                    slot: field_slot,
                    init: field_get,
                },
                ty: Type::UNIT,
                span,
            });
            allocs.push(field_alloc);

            // Register named bindings in the analysis context
            if !field.is_wildcard {
                let binding_name = field.binding_name.unwrap_or(field.field_name);
                ctx.insert_local(
                    binding_name,
                    LocalVar {
                        slot: field_slot,
                        ty: field_type,
                        is_mut: field.is_mut,
                        span,
                        allow_unused: false,
                    },
                );
            }
            // Wildcard fields: StorageLive is emitted at the outer scope, so the
            // CFG builder will drop them at scope exit. They're not in ctx.locals,
            // so the user can't reference them.
        }

        // Inner block: contains allocs (no StorageLive, so scope will be empty)
        let unit = air.add_inst(AirInst {
            data: AirInstData::UnitConst,
            ty: Type::UNIT,
            span,
        });
        let allocs_start = air.add_extra(&allocs.iter().map(|r| r.as_u32()).collect::<Vec<_>>());
        let inner_block = air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start: allocs_start,
                stmts_len: allocs.len() as u32,
                value: unit,
            },
            ty: Type::UNIT,
            span,
        });

        // Outer block: stmts are all StorageLive (wrapper block, no scope push)
        let sl_start = air.add_extra(&storage_lives.iter().map(|r| r.as_u32()).collect::<Vec<_>>());
        let outer_block = air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start: sl_start,
                stmts_len: storage_lives.len() as u32,
                value: inner_block,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(outer_block, Type::UNIT))
    }

    /// Analyze a variable reference.
    fn analyze_var_ref(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        name: Spur,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // First check if it's a parameter
        if let Some(param_info) = ctx.params.iter().find(|p| p.name == name) {
            // ADR-0076: a `Ref(T)` / `MutRef(T)`-typed parameter is by-pointer
            // at the LLVM level (`is_param_by_ref` returns true based on the
            // type). When read as a value, the AIR result is typed as the
            // inner `T` and codegen does the load-through-pointer.
            let ty = match param_info.ty.kind() {
                crate::types::TypeKind::Ref(id) => self.type_pool.ref_def(id),
                crate::types::TypeKind::MutRef(id) => self.type_pool.mut_ref_def(id),
                _ => param_info.ty,
            };
            let name_str = self.interner.resolve(&name);

            // Check if this parameter has been moved
            if let Some(move_state) = ctx.moved_vars.get(&name)
                && let Some(moved_span) = move_state.is_any_part_moved()
            {
                return Err(CompileError::use_after_move(name_str, span, moved_span));
            }

            // ADR-0076 collapse: read ref-ness off the type pool. A
            // `Ref(T)` parameter behaves like the legacy `borrow` mode
            // (move-out is rejected); a `MutRef(T)` parameter behaves
            // like the legacy `inout` mode. The "is_type_copy" check
            // consults the *referent* `T` because the move semantics
            // are about ownership of the underlying data, not the
            // ref-handle itself.
            let by_ref_kind = match param_info.ty.kind() {
                crate::types::TypeKind::Ref(_) => Some(false),
                crate::types::TypeKind::MutRef(_) => Some(true),
                _ => None,
            };
            // For move-tracking purposes, treat a `Ref(T)` / `MutRef(T)`
            // binding as if it were a non-Copy `T` (so reads need to
            // be borrows to avoid an "implicit move"). For non-ref
            // types, fall back to the actual type's Copy-ness.
            let needs_move_check = match by_ref_kind {
                Some(_) => !self.is_type_copy(ty),
                None => !self.is_type_copy(param_info.ty),
            };
            if needs_move_check {
                let is_borrow = matches!(by_ref_kind, Some(false))
                    || matches!(param_info.mode, RirParamMode::Ref);
                if is_borrow {
                    if ctx.borrow_arg_skip_move != Some(name) {
                        let name_str = self.interner.resolve(&name);
                        return Err(CompileError::new(
                            ErrorKind::MoveOutOfBorrow {
                                variable: name_str.to_string(),
                            },
                            span,
                        ));
                    }
                    return Ok(AnalysisResult::new(
                        air.add_inst(AirInst {
                            data: AirInstData::Param {
                                index: param_info.abi_slot,
                            },
                            ty,
                            span,
                        }),
                        ty,
                    ));
                }
                ctx.moved_vars
                    .entry(name)
                    .or_default()
                    .mark_path_moved(&[], span);
            }

            // Zero-sized parameter types (e.g. empty structs used as ZST
            // callables) take no ABI slot. Emit a synthetic zero-value
            // instead of a `Param { index }` that would be out-of-range at
            // codegen time.
            let air_ref = if self.abi_slot_count(ty) == 0 {
                self.emit_zst_value(air, ty, span)
            } else {
                air.add_inst(AirInst {
                    data: AirInstData::Param {
                        index: param_info.abi_slot,
                    },
                    ty,
                    span,
                })
            };
            return Ok(AnalysisResult::new(air_ref, ty));
        }

        // Look up the variable in locals
        let name_str = self.interner.resolve(&name);

        // Check if this is a local variable first
        if let Some(local) = ctx.locals.get(&name) {
            let ty = local.ty;
            let slot = local.slot;

            // Check if this variable has been moved
            if let Some(move_state) = ctx.moved_vars.get(&name)
                && let Some(moved_span) = move_state.is_any_part_moved()
            {
                return Err(CompileError::use_after_move(name_str, span, moved_span));
            }

            // If type is not Copy, mark as moved
            if !self.is_type_copy(ty) {
                ctx.moved_vars
                    .entry(name)
                    .or_default()
                    .mark_path_moved(&[], span);
            }

            // Mark variable as used
            ctx.used_locals.insert(name);

            // Load the variable
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Load { slot },
                ty,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, ty));
        }

        // Check if it's a comptime type variable (e.g., `let P = Point();`)
        // These are stored in comptime_type_vars, not in locals
        if let Some(&ty) = ctx.comptime_type_vars.get(&name) {
            // Comptime type vars produce TypeConst instructions
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::TypeConst(ty),
                ty: Type::COMPTIME_TYPE,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
        }

        // Check if it's a comptime value variable (e.g., captured `comptime N: i32`)
        // When an anonymous struct method captures comptime parameters from its enclosing function,
        // references to those parameters are resolved here and emitted as const instructions.
        if let Some(const_value) = ctx.comptime_value_vars.get(&name) {
            match const_value {
                ConstValue::Integer(val) => {
                    let ty =
                        Self::get_resolved_type(ctx, inst_ref, span, "comptime integer value")?;
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Const(*val as u64),
                        ty,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, ty));
                }
                ConstValue::Bool(val) => {
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Const(*val as u64),
                        ty: Type::BOOL,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, Type::BOOL));
                }
                ConstValue::Type(ty) => {
                    // If someone captured a type value, treat it like a type const
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::TypeConst(*ty),
                        ty: Type::COMPTIME_TYPE,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
                }
                ConstValue::ComptimeStr(_) => {
                    return Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: "comptime_str values cannot be used in runtime expressions"
                                .to_string(),
                        },
                        span,
                    ));
                }
                ConstValue::Unit => {
                    return Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: "comptime unit values cannot be used in runtime expressions"
                                .to_string(),
                        },
                        span,
                    ));
                }
                ConstValue::Struct(_)
                | ConstValue::Array(_)
                | ConstValue::EnumVariant { .. }
                | ConstValue::EnumData { .. }
                | ConstValue::EnumStruct { .. } => {
                    return Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: "comptime composite values cannot be used in runtime expressions; use @field to access fields".to_string(),
                        },
                        span,
                    ));
                }
                ConstValue::BreakSignal | ConstValue::ContinueSignal | ConstValue::ReturnSignal => {
                    unreachable!("control-flow signal in comptime_value_vars")
                }
            }
        }

        // Check if it's a constant (e.g., `const VALUE = 42` or `const math = @import("math")`)
        if let Some(const_info) = self.constants.get(&name).cloned() {
            let ty = const_info.ty;
            // For module constants, produce a TypeConst with the module type.
            // This allows field access on the module (e.g., `math.add(1, 2)`)
            if matches!(ty.kind(), TypeKind::Module(_)) {
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(ty),
                    ty,
                    span,
                });
                return Ok(AnalysisResult::new(air_ref, ty));
            }
            // For regular constants (e.g., `const VALUE = 42`), we need to inline the value.
            // We read the RIR instruction directly since type inference hasn't run on const
            // initializers in the declaration phase.
            let init_inst = self.rir.get(const_info.init);
            match &init_inst.data {
                gruel_rir::InstData::IntConst(value) => {
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Const(*value),
                        ty,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, ty));
                }
                gruel_rir::InstData::BoolConst(value) => {
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::BoolConst(*value),
                        ty: Type::BOOL,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, Type::BOOL));
                }
                _ => {
                    // For complex expressions, fall back to analyzing the init expression
                    // This may fail for expressions that need type inference context
                    return self.analyze_inst(air, const_info.init, ctx);
                }
            }
        }

        // Check if this is a type name (for comptime type parameters)
        // Try to resolve it as a type - if successful, emit a TypeConst instruction
        if let Ok(resolved_type) = self.resolve_type(name, span) {
            // This is a type name being used as a value (e.g., `i32` passed to `comptime T: type`)
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::TypeConst(resolved_type),
                ty: Type::COMPTIME_TYPE,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
        }

        // Check if this is a module-level constant (e.g., `const utils = @import("utils")`)
        // Constants are stored in self.constants and their initializers need to be analyzed
        // on first access to determine their type (lazy evaluation per ADR-0026).
        if let Some(const_info) = self.constants.get(&name).cloned() {
            // Analyze the constant's initializer to get the actual type
            // This is where @import expressions get resolved into Type::Module
            let init_result = self.analyze_inst(air, const_info.init, ctx)?;
            return Ok(init_result);
        }

        // Not a parameter, local, type, or constant - undefined variable
        Err(CompileError::new(
            ErrorKind::UndefinedVariable(name_str.to_string()),
            span,
        ))
    }

    /// Analyze a parameter reference.
    fn analyze_param_ref(
        &mut self,
        air: &mut Air,
        name: Spur,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let name_str = self.interner.resolve(&name);
        let param_info = ctx
            .params
            .iter()
            .find(|p| p.name == name)
            .ok_or_compile_error(ErrorKind::UndefinedVariable(name_str.to_string()), span)?;

        // ADR-0076: auto-deref `Ref(T)` / `MutRef(T)` parameters when read
        // as a value — codegen's `is_param_by_ref` will load through the
        // by-pointer ABI based on the parameter's *declared* type.
        let ty = match param_info.ty.kind() {
            crate::types::TypeKind::Ref(id) => self.type_pool.ref_def(id),
            crate::types::TypeKind::MutRef(id) => self.type_pool.mut_ref_def(id),
            _ => param_info.ty,
        };

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Param {
                index: param_info.abi_slot,
            },
            ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, ty))
    }

    /// Analyze an assignment.
    fn analyze_assign(
        &mut self,
        air: &mut Air,
        name: Spur,
        value: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let name_str = self.interner.resolve(&name);

        // First check if it's a parameter (for inout params)
        if let Some(param_info) = ctx.params.iter().find(|p| p.name == name) {
            // ADR-0076 collapse: read ref-ness off the type pool. A param
            // of type `MutRef(T)` permits bare-name write-through; a param
            // of type `Ref(T)` rejects it as a borrow mutation. The legacy
            // `MutRef` / `Ref` parameter modes (transport for interface
            // params per ADR-0076) still flow through for pre-collapse
            // callers.
            let by_ref_kind = match param_info.ty.kind() {
                TypeKind::MutRef(_) => Some(true),
                TypeKind::Ref(_) => Some(false),
                _ => None,
            };
            match (by_ref_kind, param_info.mode) {
                (Some(true), _) | (_, RirParamMode::MutRef) => {
                    // `MutRef(T)` param (type-driven) OR legacy `MutRef`
                    // mode (interface transport) — write-through.
                }
                (Some(false), _) | (_, RirParamMode::Ref) => {
                    return Err(CompileError::new(
                        ErrorKind::MutateBorrowedValue {
                            variable: name_str.to_string(),
                        },
                        span,
                    ));
                }
                (_, RirParamMode::Normal | RirParamMode::Comptime) => {
                    return Err(CompileError::new(
                        ErrorKind::AssignToImmutable(name_str.to_string()),
                        span,
                    )
                    .with_help(format!(
                        "parameter `{}` of type `{}` is not mutable; declare it as `{}: MutRef({})` to allow write-through",
                        name_str,
                        param_info.ty.name(),
                        name_str,
                        param_info.ty.name()
                    )));
                }
            }

            let abi_slot = param_info.abi_slot;

            // Analyze the value
            let value_result = self.analyze_inst(air, value, ctx)?;

            // Assignment to a parameter resets its move state
            ctx.moved_vars.remove(&name);

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::ParamStore {
                    param_slot: abi_slot,
                    value: value_result.air_ref,
                },
                ty: Type::UNIT,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::UNIT));
        }

        // Look up local variable
        let local = ctx
            .locals
            .get(&name)
            .ok_or_compile_error(ErrorKind::UndefinedVariable(name_str.to_string()), span)?;
        let local_slot = local.slot;
        let local_ty = local.ty;
        let local_is_mut = local.is_mut;
        let local_span = local.span;

        // ADR-0076 Phase 3: a local binding of type `MutRef(T)` supports
        // bare-name write-through — `r = v` stores `v` (typed as `T`)
        // through the pointer held in `r`'s slot. The binding mutability
        // (`let r` vs `let mut r`) does not gate this; rebinding a ref
        // is unsupported, so the only meaningful read of `r = v` is
        // write-through.
        if let TypeKind::MutRef(referent_id) = local_ty.kind() {
            let referent_ty = self.type_pool.mut_ref_def(referent_id);
            let value_result = self.analyze_inst(air, value, ctx)?;
            // Through-write is governed by the referent's type; the binding
            // identity is unchanged so no `moved_vars` book-keeping fires.
            if value_result.ty != referent_ty && value_result.ty != Type::ERROR {
                return Err(CompileError::type_mismatch(
                    self.format_type_name(referent_ty),
                    self.format_type_name(value_result.ty),
                    span,
                ));
            }
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::RefStore {
                    slot: local_slot,
                    value: value_result.air_ref,
                },
                ty: Type::UNIT,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::UNIT));
        }

        // A `Ref(T)`-typed local is a read-only ref; bare-name assign is
        // a compile-time error (mirrors the borrow-mode param case).
        if let TypeKind::Ref(_) = local_ty.kind() {
            return Err(CompileError::new(
                ErrorKind::MutateBorrowedValue {
                    variable: name_str.to_string(),
                },
                span,
            ));
        }

        // Check mutability
        if !local_is_mut {
            return Err(CompileError::new(
                ErrorKind::AssignToImmutable(name_str.to_string()),
                span,
            )
            .with_label("variable declared as immutable here", local_span)
            .with_help(format!(
                "consider making `{}` mutable: `let mut {}`",
                name_str, name_str
            )));
        }

        let slot = local_slot;

        // Analyze the value
        let value_result = self.analyze_inst(air, value, ctx)?;

        // Determine if the slot had a live value before this assignment.
        // If the variable is not in moved_vars, the slot contains a value that has
        // not been moved away, so it needs to be dropped before the new value is written.
        let had_live_value = !ctx.moved_vars.contains_key(&name);

        // Assignment to a mutable variable resets its move state.
        ctx.moved_vars.remove(&name);

        // Emit store instruction
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Store {
                slot,
                value: value_result.air_ref,
                had_live_value,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
    }

    // ========================================================================
    // Struct operations: StructDecl, StructInit, FieldGet, FieldSet
    // ========================================================================

    /// Analyze a struct operation instruction.
    ///
    /// Handles: StructDecl, StructInit, FieldGet, FieldSet
    pub(crate) fn analyze_struct_ops(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::StructDecl { .. } => {
                // Struct declarations are handled at the top level
                Err(CompileError::new(
                    ErrorKind::InternalError(
                        "StructDecl should not appear in expression context".to_string(),
                    ),
                    inst.span,
                ))
            }

            InstData::StructInit {
                type_name,
                fields_start,
                fields_len,
                ..
            } => self.analyze_struct_init(
                air,
                *type_name,
                *fields_start,
                *fields_len,
                inst.span,
                ctx,
            ),

            InstData::FieldGet { base, field } => {
                self.analyze_field_get(air, inst_ref, *base, *field, inst.span, ctx)
            }

            InstData::FieldSet { base, field, value } => {
                self.analyze_field_set(air, *base, *field, *value, inst.span, ctx)
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_struct_ops called with non-struct instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }

    /// Analyze a struct initialization.
    fn analyze_struct_init(
        &mut self,
        air: &mut Air,
        type_name: Spur,
        fields_start: u32,
        fields_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let field_inits = self.rir.get_field_inits(fields_start, fields_len);
        // Look up the struct type
        // First check if it's a comptime type variable (e.g., `let Point = make_point(); Point { ... }`)
        let type_name_str = self.interner.resolve(&type_name);
        let struct_id = if let Some(&ty) = ctx.comptime_type_vars.get(&type_name) {
            // Extract struct ID from the comptime type
            match ty.kind() {
                TypeKind::Struct(id) => id,
                _ => {
                    return Err(CompileError::type_mismatch(
                        "struct type".to_string(),
                        ty.name().to_string(),
                        span,
                    ));
                }
            }
        } else {
            *self
                .structs
                .get(&type_name)
                .ok_or_compile_error(ErrorKind::UnknownType(type_name_str.to_string()), span)?
        };

        // Get struct def (returns owned copy from pool)
        let struct_def = self.type_pool.struct_def(struct_id);
        let struct_type = Type::new_struct(struct_id);

        // Build a map from field name to struct field index
        let field_index_map: rustc_hash::FxHashMap<&str, usize> = struct_def
            .fields
            .iter()
            .enumerate()
            .map(|(i, f)| (f.name.as_str(), i))
            .collect();

        // Check for unknown or duplicate fields
        let mut seen_fields = rustc_hash::FxHashSet::default();
        for (init_field_name, _) in field_inits.iter() {
            let init_name = self.interner.resolve(init_field_name);

            if !field_index_map.contains_key(init_name) {
                return Err(CompileError::new(
                    ErrorKind::UnknownField {
                        struct_name: struct_def.name.clone(),
                        field_name: init_name.to_string(),
                    },
                    span,
                ));
            }

            if !seen_fields.insert(init_name) {
                return Err(CompileError::new(
                    ErrorKind::DuplicateField {
                        struct_name: struct_def.name.clone(),
                        field_name: init_name.to_string(),
                    },
                    span,
                ));
            }

            // ADR-0073: unified visibility check on each field referenced
            // by the struct literal.
            let field_idx = field_index_map[init_name];
            let struct_field = &struct_def.fields[field_idx];
            self.check_field_visibility(&struct_def, struct_field, span)?;
        }

        // Check that all fields are provided
        if field_inits.len() != struct_def.fields.len() {
            let missing_fields: Vec<String> = struct_def
                .fields
                .iter()
                .filter(|f| !seen_fields.contains(f.name.as_str()))
                .map(|f| f.name.clone())
                .collect();
            return Err(CompileError::new(
                ErrorKind::MissingFields(Box::new(MissingFieldsError {
                    struct_name: struct_def.name.clone(),
                    missing_fields,
                })),
                span,
            ));
        }

        // Analyze field values in SOURCE ORDER (left-to-right as written)
        let mut analyzed_fields: Vec<Option<AirRef>> = vec![None; struct_def.fields.len()];
        let mut source_order: Vec<usize> = Vec::with_capacity(field_inits.len());

        for (init_field_name, field_value) in field_inits.iter() {
            let init_name = self.interner.resolve(init_field_name);
            let field_idx = field_index_map[init_name];
            let expected_field_type = struct_def.fields[field_idx].ty;

            // Check if this is an integer literal that needs type coercion
            // This handles the case where HM inference couldn't resolve the type
            // (e.g., when the struct comes from a comptime type variable)
            let field_inst = self.rir.get(*field_value);
            let field_result = if let InstData::IntConst(value) = &field_inst.data {
                // Integer literal - use the expected field type directly
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Const(*value),
                    ty: expected_field_type,
                    span: field_inst.span,
                });
                AnalysisResult::new(air_ref, expected_field_type)
            } else {
                // Not an integer literal - analyze normally
                self.analyze_inst(air, *field_value, ctx)?
            };

            // Type check the field value against the expected type
            if field_result.ty != expected_field_type {
                return Err(CompileError::type_mismatch(
                    expected_field_type.name().to_string(),
                    field_result.ty.name().to_string(),
                    span,
                )
                .with_label(
                    format!(
                        "field '{}' expects type {}",
                        init_name,
                        expected_field_type.name()
                    ),
                    span,
                ));
            }

            analyzed_fields[field_idx] = Some(field_result.air_ref);
            source_order.push(field_idx);
        }

        // Collect field refs in DECLARATION ORDER
        let field_refs: Vec<AirRef> = analyzed_fields
            .into_iter()
            .map(|opt| opt.expect("all fields should be initialized"))
            .collect();

        // Encode into extra array
        let fields_len = field_refs.len() as u32;
        let field_u32s: Vec<u32> = field_refs.iter().map(|r| r.as_u32()).collect();
        let fields_start = air.add_extra(&field_u32s);
        let source_order_u32s: Vec<u32> = source_order.iter().map(|&i| i as u32).collect();
        let source_order_start = air.add_extra(&source_order_u32s);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::StructInit {
                struct_id,
                fields_start,
                fields_len,
                source_order_start,
            },
            ty: struct_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, struct_type))
    }

    /// Analyze a tuple literal (ADR-0048).
    ///
    /// Tuples are lowered to anonymous structs with field names "0", "1", ...
    /// This reuses the existing structural dedup in `find_or_create_anon_struct`,
    /// so two tuples with the same element types are the same struct type.
    pub(crate) fn analyze_tuple_init(
        &mut self,
        air: &mut Air,
        elems_start: u32,
        elems_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Pull element inst refs out of the extra array.
        let elem_refs: Vec<InstRef> = self.rir.get_inst_refs(elems_start, elems_len).to_vec();

        // Analyze each element in source order.
        let mut elem_results = Vec::with_capacity(elem_refs.len());
        for elem_ref in &elem_refs {
            let result = self.analyze_inst(air, *elem_ref, ctx)?;
            elem_results.push(result);
        }

        // Build synthetic struct fields named "0", "1", ... with the element types.
        let struct_fields: Vec<StructField> = elem_results
            .iter()
            .enumerate()
            .map(|(i, r)| StructField {
                name: i.to_string(),
                ty: r.ty,

                is_pub: true,
            })
            .collect();

        // Find or create the anon struct via the existing structural-dedup helper.
        let (struct_ty, _is_new) =
            self.find_or_create_anon_struct(&struct_fields, &[], &HashMap::default());
        let struct_id = struct_ty
            .as_struct()
            .expect("find_or_create_anon_struct returned non-struct type");

        // Field refs are already in declaration order (i == field_index), and source
        // order matches. Encode both into AIR's extra array.
        let field_u32s: Vec<u32> = elem_results.iter().map(|r| r.air_ref.as_u32()).collect();
        let fields_start_air = air.add_extra(&field_u32s);
        let source_order_u32s: Vec<u32> = (0..elem_results.len() as u32).collect();
        let source_order_start = air.add_extra(&source_order_u32s);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::StructInit {
                struct_id,
                fields_start: fields_start_air,
                fields_len: elem_results.len() as u32,
                source_order_start,
            },
            ty: struct_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, struct_ty))
    }

    /// Analyze an anonymous function value (ADR-0055).
    ///
    /// Desugars `fn(params) -> ret { body }` into:
    /// - a fresh anon struct with 0 fields and a single `__call` method
    /// - an empty `StructInit` against that struct
    ///
    /// Phase 2 uses the normal structural-dedup path, so two same-signature
    /// anonymous functions will collide (known limitation); Phase 3 adds
    /// lambda-origin tagging so each site is unique.
    pub(crate) fn analyze_anon_fn_value(
        &mut self,
        air: &mut Air,
        method_ref: InstRef,
        span: Span,
        _ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Extract the `__call` method signature from the FnDecl the parser
        // synthesized for us.
        let method_inst = self.rir.get(method_ref);
        let (call_name, param_types_syms, return_type_sym, has_self) = match &method_inst.data {
            InstData::FnDecl {
                name,
                params_start,
                params_len,
                return_type,
                has_self,
                ..
            } => {
                let params = self.rir.get_params(*params_start, *params_len);
                let param_types: Vec<Spur> = params.iter().map(|p| p.ty).collect();
                (*name, param_types, *return_type, *has_self)
            }
            _ => unreachable!("AnonFnValue method must be a FnDecl (invariant set by astgen)"),
        };

        let method_sig = super::AnonMethodSig {
            name: call_name,
            has_self,
            param_types: param_types_syms,
            return_type: return_type_sym,
        };

        // Synthesize a *unique* anon struct with 0 fields and this single
        // method. Each `fn(...)` source site must produce a distinct type —
        // two same-signature anonymous functions with different bodies would
        // otherwise dedup into the same type and the compiler would not be
        // able to tell which body to call.
        let struct_fields: Vec<StructField> = Vec::new();
        let struct_ty = self.create_unique_anon_struct(
            &struct_fields,
            std::slice::from_ref(&method_sig),
            &HashMap::default(),
        );
        let struct_id = struct_ty
            .as_struct()
            .expect("create_unique_anon_struct returned non-struct type");

        // Register the __call method on the new struct so it is callable.
        self.register_anon_fn_call_method(struct_id, struct_ty, method_ref, span)?;

        // Emit an empty StructInit — the struct has no fields, so both the
        // field array and the source-order array are zero-length.
        let fields_start = air.add_extra(&[]);
        let source_order_start = air.add_extra(&[]);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::StructInit {
                struct_id,
                fields_start,
                fields_len: 0,
                source_order_start,
            },
            ty: struct_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, struct_ty))
    }

    /// Materialize a zero-sized value of the given type as AIR. Used for
    /// ZST parameters (empty-struct callables, `()`, and similar) where
    /// emitting an ABI `Param` would dereference a non-existent slot at
    /// codegen time.
    fn emit_zst_value(&mut self, air: &mut Air, ty: Type, span: Span) -> crate::inst::AirRef {
        match ty.kind() {
            TypeKind::Struct(struct_id) => {
                let fields_start = air.add_extra(&[]);
                let source_order_start = air.add_extra(&[]);
                air.add_inst(AirInst {
                    data: AirInstData::StructInit {
                        struct_id,
                        fields_start,
                        fields_len: 0,
                        source_order_start,
                    },
                    ty,
                    span,
                })
            }
            _ => {
                // Unit, Never, and other non-struct ZSTs: fall back to a
                // unit constant. Never-typed values are produced via
                // divergence, not via VarRef, so this branch in practice
                // only fires for Unit and related marker types.
                air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty,
                    span,
                })
            }
        }
    }

    /// ADR-0055 call-sugar: rewrite `f(args)` as `f.__call(args)` when `f` is
    /// a local or parameter whose type is a struct with a `__call` method.
    ///
    /// Returns `None` if `name` is not such a binding — the caller should fall
    /// through to the normal function-lookup path. `Some(result)` means we
    /// handled the call (successfully or not).
    fn try_analyze_call_sugar(
        &mut self,
        air: &mut Air,
        name: Spur,
        args_start: u32,
        args_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> Option<CompileResult<AnalysisResult>> {
        // Resolve `name` to a receiver, checking locals first (they shadow
        // parameters in all contexts).
        let (receiver_ty, source) = if let Some(local) = ctx.locals.get(&name) {
            (local.ty, ReceiverSource::Local(local.slot))
        } else if let Some(param) = ctx.params.iter().find(|p| p.name == name) {
            (param.ty, ReceiverSource::Param(param.abi_slot))
        } else {
            return None;
        };

        let struct_id = match receiver_ty.kind() {
            TypeKind::Struct(id) => id,
            _ => return None,
        };

        let call_sym = self.interner.get("__call")?;
        if !self.methods.contains_key(&(struct_id, call_sym)) {
            return None;
        }

        Some(self.emit_call_sugar(
            air,
            CallSugarReceiver {
                name,
                ty: receiver_ty,
                source,
                struct_id,
            },
            call_sym,
            (args_start, args_len),
            span,
            ctx,
        ))
    }

    /// Emit the AIR for a successful call-sugar rewrite. Factored out of
    /// `try_analyze_call_sugar` so that function can stay a simple predicate.
    fn emit_call_sugar(
        &mut self,
        air: &mut Air,
        receiver: CallSugarReceiver,
        call_sym: Spur,
        (args_start, args_len): (u32, u32),
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let CallSugarReceiver {
            name: receiver_name,
            ty: receiver_ty,
            source,
            struct_id,
        } = receiver;
        // Emit the receiver load, tracking move/use state the same way
        // `analyze_var_ref` does for ordinary VarRef expressions.
        let receiver_name_str = self.interner.resolve(&receiver_name).to_string();
        if let Some(move_state) = ctx.moved_vars.get(&receiver_name)
            && let Some(moved_span) = move_state.is_any_part_moved()
        {
            return Err(CompileError::use_after_move(
                receiver_name_str,
                span,
                moved_span,
            ));
        }
        if !self.is_type_copy(receiver_ty) {
            ctx.moved_vars
                .entry(receiver_name)
                .or_default()
                .mark_path_moved(&[], span);
        }
        ctx.used_locals.insert(receiver_name);

        let receiver_air_ref = match source {
            ReceiverSource::Local(slot) => {
                if self.abi_slot_count(receiver_ty) == 0 {
                    // ZST local — no slot to load from, materialize zero.
                    self.emit_zst_value(air, receiver_ty, span)
                } else {
                    air.add_inst(AirInst {
                        data: AirInstData::Load { slot },
                        ty: receiver_ty,
                        span,
                    })
                }
            }
            ReceiverSource::Param(abi_slot) => {
                if self.abi_slot_count(receiver_ty) == 0 {
                    self.emit_zst_value(air, receiver_ty, span)
                } else {
                    air.add_inst(AirInst {
                        data: AirInstData::Param { index: abi_slot },
                        ty: receiver_ty,
                        span,
                    })
                }
            }
        };

        // Look up the method so we can pass its info through unchanged.
        let method_info = self
            .methods
            .get(&(struct_id, call_sym))
            .expect("__call method was present in the predicate");
        let return_type = method_info.return_type;
        let method_param_types = self.param_arena.types(method_info.params).to_vec();
        let is_unchecked = method_info.is_unchecked;
        let has_self = method_info.has_self;

        // ADR-0026 lazy analysis: track this `__call` method as referenced
        // so its body is analyzed by the work queue. Otherwise the
        // anonymous-struct method ends up registered in `self.methods`
        // but never analyzed, producing a link error at codegen time.
        ctx.referenced_methods.insert((struct_id, call_sym));

        // `__call` must be a method (has self) — we enforce it in astgen, but
        // defensive check keeps the failure mode clear for user-defined
        // callable types.
        if !has_self {
            let struct_name = self.type_pool.struct_def(struct_id).name.clone();
            return Err(CompileError::new(
                ErrorKind::AssocFnCalledAsMethod {
                    type_name: struct_name,
                    function_name: "__call".to_string(),
                },
                span,
            ));
        }

        if is_unchecked && ctx.checked_depth == 0 {
            let struct_name = self.type_pool.struct_def(struct_id).name.clone();
            return Err(CompileError::new(
                ErrorKind::UncheckedCallRequiresChecked(format!("{}.__call", struct_name)),
                span,
            ));
        }

        let args = self.rir.get_call_args(args_start, args_len);
        if args.len() != method_param_types.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: method_param_types.len(),
                    found: args.len(),
                },
                span,
            ));
        }

        self.check_exclusive_access(&args, span)?;

        let mut air_args = vec![AirCallArg {
            value: receiver_air_ref,
            mode: AirArgMode::Normal,
        }];
        air_args.extend(self.analyze_call_args(air, &args, ctx)?);

        let struct_name_str = self.type_pool.struct_def(struct_id).name.clone();
        let call_name = format!("{}.__call", struct_name_str);
        let call_name_sym = self.interner.get_or_intern(&call_name);

        let mut extra_data = Vec::with_capacity(air_args.len() * 2);
        for arg in &air_args {
            extra_data.push(arg.value.as_u32());
            extra_data.push(arg.mode.as_u32());
        }
        let args_len_u32 = air_args.len() as u32;
        let args_start_air = air.add_extra(&extra_data);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: call_name_sym,
                args_start: args_start_air,
                args_len: args_len_u32,
            },
            ty: return_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, return_type))
    }

    /// Analyze a field access.
    ///
    /// Uses place-based analysis (ADR-0030) when possible for efficient code generation.
    fn analyze_field_get(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        base: InstRef,
        field: Spur,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // First, check if the base is a module access (special case, not a place)
        // We need to peek at the base type to detect module.Type access patterns.
        let base_inst = self.rir.get(base);
        if let InstData::VarRef { name } = &base_inst.data {
            // Check if this VarRef refers to a module
            if let Some(local) = ctx.locals.get(name)
                && local.ty.as_module().is_some()
            {
                // This is module.Member access - handle specially
                let module_id = local.ty.as_module().unwrap();
                return self.analyze_module_type_member_access(air, module_id, field, span);
            }
        }

        // Try to trace this expression to a place (lvalue)
        if let Some(trace) = self.try_trace_place(inst_ref, air, ctx)? {
            let field_type = trace.result_type();

            // Check if the root variable was fully moved (applies regardless of field type)
            if let Some(state) = ctx.moved_vars.get(&trace.root_var)
                && let Some(moved_span) = state.full_move
            {
                let root_name = self.interner.resolve(&trace.root_var);
                return Err(CompileError::use_after_move(root_name, span, moved_span));
            }

            // Get struct info for move checking
            // The trace's result type is the field type, but we need the parent struct type
            // to check if it's linear. The parent is the type *before* the last projection.
            let parent_type = if trace.projections.len() > 1 {
                trace.projections[trace.projections.len() - 2].result_type
            } else {
                trace.base_type
            };

            let is_linear = parent_type
                .as_struct()
                .map(|id| self.type_pool.struct_def(id).posture == Posture::Linear)
                .unwrap_or(false);

            // ADR-0081: detect projection through a `Ref(T)` / `MutRef(T)`
            // root binding. The trace's `is_borrow_param` flag covers
            // `Ref(_)` parameters (per `param_kind_flags`); we additionally
            // walk the params and locals to catch `MutRef(_)` bindings,
            // which `param_kind_flags` reports as "mutable" rather than
            // "borrow". In either case, field access is a borrow
            // projection — never a move — so ADR-0036's partial-move ban
            // does not apply.
            let projects_through_ref = trace.is_borrow_param
                || ctx.locals.get(&trace.root_var).is_some_and(|local| {
                    matches!(
                        local.ty.kind(),
                        crate::types::TypeKind::Ref(_) | crate::types::TypeKind::MutRef(_)
                    )
                })
                || ctx.params.iter().any(|p| {
                    p.name == trace.root_var
                        && matches!(
                            p.ty.kind(),
                            crate::types::TypeKind::Ref(_) | crate::types::TypeKind::MutRef(_)
                        )
                });

            // Move checking using the trace
            if is_linear {
                // For linear types, field access consumes the entire struct
                ctx.moved_vars
                    .entry(trace.root_var)
                    .or_default()
                    .mark_path_moved(&[], span);
            } else if !self.is_type_copy(field_type) && !projects_through_ref {
                // ADR-0036: partial field moves are normally banned because
                // the CFG drops the whole local on scope exit and has no
                // mechanism to skip moved fields. We carve out a narrow
                // exception:
                //
                //   1. The root being moved is `self` (a by-value receiver
                //      of a consuming method).
                //   2. Every other field of `self`'s struct is Copy.
                //
                // Under those conditions, moving `self.field` is equivalent
                // to consuming `self` entirely — the remaining Copy fields
                // need no drop, and `self` is going out of scope at the
                // method's return anyway. This makes thin one-field
                // wrappers (`pub fn dispose(self) { self.inner.dispose() }`)
                // work without the user writing the destructure form by hand.
                //
                // The restriction to `self` is what keeps the rule from
                // re-introducing the partial-move unsoundness ADR-0036 was
                // designed to prevent: ordinary `consume(p.a); p.b` still
                // errors as before.
                let self_sym = self.interner.get("self");
                let root_is_self = self_sym == Some(trace.root_var);
                let parent_struct_def = parent_type
                    .as_struct()
                    .map(|id| self.type_pool.struct_def(id));
                let other_fields_all_copy = parent_struct_def
                    .as_ref()
                    .map(|def| {
                        def.fields.iter().all(|f| {
                            f.name == self.interner.resolve(&field)
                                || f.ty.is_copy_in_pool(&self.type_pool)
                        })
                    })
                    .unwrap_or(false);

                if root_is_self && other_fields_all_copy {
                    // Treat the partial move as a full move of `self`. All
                    // remaining fields are Copy, so the whole-variable
                    // drop emitted at function exit is a no-op —
                    // equivalent to the user writing
                    // `let SelfType { field, .. } = self;` at the top of
                    // the method.
                    ctx.moved_vars
                        .entry(trace.root_var)
                        .or_default()
                        .mark_path_moved(&[], span);
                } else {
                    // ADR-0036: cannot synthesize a partial drop here.
                    // Surface the explicit-destructure error.
                    let is_tuple = parent_struct_def
                        .as_ref()
                        .map(|def| def.is_tuple_shaped())
                        .unwrap_or(false);
                    let type_name = parent_struct_def
                        .as_ref()
                        .and_then(|def| {
                            let pool = &self.type_pool;
                            def.tuple_display_name(|ty| ty.safe_name_with_pool(Some(pool)))
                                .or_else(|| Some(def.name.clone()))
                        })
                        .unwrap_or_else(|| parent_type.name().to_string());
                    let field_name = self.interner.resolve(&field).to_string();
                    let help = if is_tuple {
                        let arity = parent_struct_def
                            .as_ref()
                            .map(|def| def.fields.len())
                            .unwrap_or(0);
                        let bindings: Vec<String> = (0..arity).map(|i| format!("x{}", i)).collect();
                        format!("use destructuring: `let ({}) = ...;`", bindings.join(", "))
                    } else {
                        format!(
                            "use destructuring: `let {type_name} {{ {field_name}, .. }} = ...;`"
                        )
                    };
                    return Err(CompileError::new(
                        ErrorKind::CannotMoveField {
                            type_name: type_name.clone(),
                            field: field_name.clone(),
                        },
                        span,
                    )
                    .with_help(help));
                }
            }

            // Mark the root variable as used so unused-variable analysis
            // accounts for tuple/struct field projections.
            ctx.used_locals.insert(trace.root_var);

            // Emit PlaceRead instruction
            let place_ref = Self::build_place_ref(air, &trace);
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::PlaceRead { place: place_ref },
                ty: field_type,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, field_type));
        }

        // Fallback: base is not a place (e.g., function call result)
        // Spill the computed value to a temporary, then use PlaceRead.
        // This handles `get_struct().field` patterns.
        let base_result = self.analyze_inst(air, base, ctx)?;
        let base_type = base_result.ty;

        // Handle module member access that wasn't caught above
        if let Some(module_id) = base_type.as_module() {
            return self.analyze_module_type_member_access(air, module_id, field, span);
        }

        let struct_id = match base_type.as_struct() {
            Some(id) => id,
            None => {
                return Err(CompileError::new(
                    ErrorKind::FieldAccessOnNonStruct {
                        found: base_type.name().to_string(),
                    },
                    span,
                ));
            }
        };

        let struct_def = self.type_pool.struct_def(struct_id);
        let field_name_str = self.interner.resolve(&field).to_string();

        // ADR-0048: render tuple-shaped structs with tuple syntax in errors.
        let display_struct_name = {
            let pool = &self.type_pool;
            struct_def
                .tuple_display_name(|ty| ty.safe_name_with_pool(Some(pool)))
                .unwrap_or_else(|| struct_def.name.clone())
        };
        let (field_index, struct_field) = match struct_def.find_field(&field_name_str) {
            Some(f) => {
                // ADR-0073: unified visibility check.
                self.check_field_visibility(&struct_def, f.1, span)?;
                f
            }
            None => {
                let mut err = CompileError::new(
                    ErrorKind::UnknownField {
                        struct_name: display_struct_name.clone(),
                        field_name: field_name_str.clone(),
                    },
                    span,
                );
                // ADR-0048: for tuple-shaped structs, add a specific help note.
                if struct_def.is_tuple_shaped()
                    && let Ok(idx) = field_name_str.parse::<u32>()
                {
                    err = err.with_help(format!(
                        "tuple index {} out of bounds: {} has {} element{}",
                        idx,
                        display_struct_name,
                        struct_def.fields.len(),
                        if struct_def.fields.len() == 1 {
                            ""
                        } else {
                            "s"
                        },
                    ));
                }
                return Err(err);
            }
        };

        let field_type = struct_field.ty;

        // Allocate a temporary slot for the computed struct value
        let temp_slot = ctx.next_slot;
        let num_slots = self.abi_slot_count(base_type);
        ctx.next_slot += num_slots;

        // Emit StorageLive for the temporary
        let storage_live_ref = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot: temp_slot },
            ty: base_type,
            span,
        });

        // Emit Alloc to store the computed value
        let alloc_ref = air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot: temp_slot,
                init: base_result.air_ref,
            },
            ty: Type::UNIT,
            span,
        });

        // Create PlaceRead with Field projection on the temp slot
        let place_ref = air.make_place(
            AirPlaceBase::Local(temp_slot),
            std::iter::once(AirProjection::Field {
                struct_id,
                field_index: field_index as u32,
            }),
        );
        let read_ref = air.add_inst(AirInst {
            data: AirInstData::PlaceRead { place: place_ref },
            ty: field_type,
            span,
        });

        // Note: We don't emit StorageDead here. The temporary will be cleaned up by
        // scope-based drop elaboration in the CFG builder. This is slightly conservative
        // (temp lives until scope exit rather than immediately after use) but correct.
        // A future optimization could add explicit StorageDead at the right point.
        let stmts_start = air.add_extra(&[storage_live_ref.as_u32(), alloc_ref.as_u32()]);
        let block_ref = air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start,
                stmts_len: 2,
                value: read_ref,
            },
            ty: field_type,
            span,
        });
        Ok(AnalysisResult::new(block_ref, field_type))
    }

    /// Analyze a field assignment.
    ///
    /// This is a complex operation that handles VarRef, ParamRef, and chained field access.
    /// The full implementation is in analysis.rs as it's quite large (~200 lines).
    fn analyze_field_set(
        &mut self,
        air: &mut Air,
        base: InstRef,
        field: Spur,
        value: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Delegate to the main implementation in analysis.rs
        // This is one of the larger handlers that we'll keep in the main file
        // for now and refactor in a future pass
        self.analyze_field_set_impl(air, base, field, value, span, ctx)
    }

    /// Analyze module type member access: `module.StructName` or `module.EnumName`.
    ///
    /// When accessing a struct or enum through a module, we return a comptime type
    /// that can be used to construct values. For example:
    ///
    /// ```gruel
    /// let utils = @import("utils");
    /// let Point = utils.Point;        // Returns Type::Struct as a comptime type
    /// let p = Point { x: 1, y: 2 };   // Uses the type to construct a value
    /// ```
    ///
    /// This enables the pattern of importing types through modules and using them
    /// for struct initialization or enum variant access.
    fn analyze_module_type_member_access(
        &mut self,
        air: &mut Air,
        module_id: crate::types::ModuleId,
        member_name: Spur,
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        let member_name_str = self.interner.resolve(&member_name).to_string();

        // Get the module definition to find its file path
        let module_def = self.module_registry.get_def(module_id);
        let module_file_path = module_def.file_path.clone();

        // Get the accessing file's directory for visibility check
        let accessing_file_path = self.get_source_path(span).map(|s| s.to_string());

        // First, try to find a struct with this name that belongs to the module's file
        if let Some(&struct_id) = self.structs.get(&member_name) {
            let struct_def = self.type_pool.struct_def(struct_id);

            // Check if this struct was defined in the module's file
            if let Some(struct_file_path) = self.get_file_path(struct_def.file_id)
                && struct_file_path == module_file_path
            {
                // Check visibility: pub structs are visible to all, private only to same directory
                if !struct_def.is_pub {
                    // Check if accessing from same directory
                    let same_dir = match &accessing_file_path {
                        Some(accessing) => {
                            let accessing_dir = std::path::Path::new(accessing).parent();
                            let module_dir = std::path::Path::new(&module_file_path).parent();
                            accessing_dir == module_dir
                        }
                        None => true, // Be permissive if we can't determine the path
                    };

                    if !same_dir {
                        return Err(CompileError::new(
                            ErrorKind::PrivateMemberAccess {
                                item_kind: "struct".to_string(),
                                name: member_name_str,
                            },
                            span,
                        ));
                    }
                }

                // Return a TypeConst instruction with the struct type
                let struct_type = Type::new_struct(struct_id);
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(struct_type),
                    ty: Type::COMPTIME_TYPE,
                    span,
                });
                return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
            }
        }

        // Next, try to find an enum with this name that belongs to the module's file
        if let Some(&enum_id) = self.enums.get(&member_name) {
            let enum_def = self.type_pool.enum_def(enum_id);

            // Check if this enum was defined in the module's file
            if let Some(enum_file_path) = self.get_file_path(enum_def.file_id)
                && enum_file_path == module_file_path
            {
                // Check visibility: pub enums are visible to all, private only to same directory
                if !enum_def.is_pub {
                    // Check if accessing from same directory
                    let same_dir = match &accessing_file_path {
                        Some(accessing) => {
                            let accessing_dir = std::path::Path::new(accessing).parent();
                            let module_dir = std::path::Path::new(&module_file_path).parent();
                            accessing_dir == module_dir
                        }
                        None => true, // Be permissive if we can't determine the path
                    };

                    if !same_dir {
                        return Err(CompileError::new(
                            ErrorKind::PrivateMemberAccess {
                                item_kind: "enum".to_string(),
                                name: member_name_str,
                            },
                            span,
                        ));
                    }
                }

                // Return a TypeConst instruction with the enum type
                let enum_type = Type::new_enum(enum_id);
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(enum_type),
                    ty: Type::COMPTIME_TYPE,
                    span,
                });
                return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
            }
        }

        // Member not found in the module
        Err(CompileError::new(
            ErrorKind::UnknownModuleMember {
                module_name: module_def.import_path.clone(),
                member_name: member_name_str,
            },
            span,
        ))
    }

    // ========================================================================
    // Array operations: ArrayInit, IndexGet, IndexSet
    // ========================================================================

    /// Analyze an array operation instruction.
    ///
    /// Handles: ArrayInit, IndexGet, IndexSet
    pub(crate) fn analyze_array_ops(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::ArrayInit {
                elems_start,
                elems_len,
            } => self.analyze_array_init(air, inst_ref, *elems_start, *elems_len, inst.span, ctx),

            InstData::IndexGet { base, index } => {
                self.analyze_index_get(air, inst_ref, *base, *index, inst.span, ctx)
            }

            InstData::IndexSet { base, index, value } => {
                self.analyze_index_set(air, *base, *index, *value, inst.span, ctx)
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_array_ops called with non-array instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }

    /// Analyze an array initialization.
    fn analyze_array_init(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        elems_start: u32,
        elems_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let elem_refs = self.rir.get_inst_refs(elems_start, elems_len);

        // Get the array type from HM inference
        let array_type = Self::get_resolved_type(ctx, inst_ref, span, "array literal")?;

        let (_array_type_id, _elem_type, expected_len) = match array_type.as_array() {
            Some(type_id) => {
                let (element_type, length) = self.type_pool.array_def(type_id);
                (type_id, element_type, length)
            }
            None => {
                return Err(CompileError::new(
                    ErrorKind::InternalError(format!(
                        "Array literal inferred as non-array type: {}",
                        array_type.name()
                    )),
                    span,
                ));
            }
        };

        // Verify length matches
        if elem_refs.len() as u64 != expected_len {
            return Err(CompileError::new(
                ErrorKind::ArrayLengthMismatch {
                    expected: expected_len,
                    found: elem_refs.len() as u64,
                },
                span,
            ));
        }

        // Analyze elements
        let mut air_elems = Vec::with_capacity(elem_refs.len());
        for elem_ref in elem_refs {
            let elem_result = self.analyze_inst(air, elem_ref, ctx)?;
            air_elems.push(elem_result.air_ref);
        }

        // Encode into extra array
        let elems_len = air_elems.len() as u32;
        let elem_u32s: Vec<u32> = air_elems.iter().map(|r| r.as_u32()).collect();
        let elems_start = air.add_extra(&elem_u32s);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::ArrayInit {
                elems_start,
                elems_len,
            },
            ty: array_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, array_type))
    }

    /// Analyze an array index read.
    ///
    /// Uses place-based analysis (ADR-0030) when possible for efficient code generation.
    fn analyze_index_get(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        base: InstRef,
        index: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // ADR-0064: indexing on a `Slice(T)` / `MutSlice(T)` value goes
        // through a dedicated runtime intrinsic instead of the array
        // place-tracing path.
        if let Some(result) = self.try_analyze_slice_index_read(air, base, index, span, ctx)? {
            return Ok(result);
        }
        // ADR-0066: indexing on a Vec(T) value emits a vec_index_read
        // intrinsic.
        if let Some(result) = self.try_analyze_vec_index_read(air, base, index, span, ctx)? {
            return Ok(result);
        }
        // Check for constant out-of-bounds index early (before tracing)
        // We need the array type for bounds checking, so peek at the base first
        let _base_inst = self.rir.get(base);

        // Try to trace this expression to a place (lvalue)
        if let Some(trace) = self.try_trace_place(inst_ref, air, ctx)? {
            let elem_type = trace.result_type();

            // Get array info from the parent type (before the last projection)
            let parent_type = if trace.projections.len() > 1 {
                trace.projections[trace.projections.len() - 2].result_type
            } else {
                trace.base_type
            };

            let array_len = match parent_type.as_array() {
                Some(type_id) => {
                    let (_elem, len) = self.type_pool.array_def(type_id);
                    len
                }
                None => {
                    // This shouldn't happen if try_trace_place worked correctly
                    return Err(CompileError::new(
                        ErrorKind::IndexOnNonArray {
                            found: parent_type.name().to_string(),
                        },
                        span,
                    ));
                }
            };

            // Check for constant out-of-bounds index
            if let Some(const_idx) = self.try_get_const_index(index)
                && (const_idx < 0 || const_idx as u64 >= array_len)
            {
                return Err(CompileError::new(
                    ErrorKind::IndexOutOfBounds {
                        index: const_idx,
                        length: array_len,
                    },
                    self.rir.get(index).span,
                ));
            }

            // Prevent moving non-Copy elements out of arrays.
            if !self.is_type_copy(elem_type) {
                return Err(CompileError::new(
                    ErrorKind::MoveOutOfIndex {
                        element_type: elem_type.name().to_string(),
                    },
                    span,
                )
                .with_help("use explicit methods like swap() or take() to remove elements"));
            }

            // Emit PlaceRead instruction
            let place_ref = Self::build_place_ref(air, &trace);
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::PlaceRead { place: place_ref },
                ty: elem_type,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, elem_type));
        }

        // Fallback: base is not a place (e.g., function call result)
        // Spill the computed array to a temporary, then use PlaceRead.
        // This handles `get_array()[i]` patterns.
        let base_result = self.analyze_inst(air, base, ctx)?;
        let base_type = base_result.ty;
        let index_result = self.analyze_inst(air, index, ctx)?;

        // Verify base is an array
        let (_array_type_id, elem_type, array_len) = match base_type.as_array() {
            Some(type_id) => {
                let (element_type, length) = self.type_pool.array_def(type_id);
                (type_id, element_type, length)
            }
            None => {
                return Err(CompileError::new(
                    ErrorKind::IndexOnNonArray {
                        found: base_type.name().to_string(),
                    },
                    span,
                ));
            }
        };

        // Check for constant out-of-bounds index
        if let Some(const_idx) = self.try_get_const_index(index)
            && (const_idx < 0 || const_idx as u64 >= array_len)
        {
            return Err(CompileError::new(
                ErrorKind::IndexOutOfBounds {
                    index: const_idx,
                    length: array_len,
                },
                self.rir.get(index).span,
            ));
        }

        // Prevent moving non-Copy elements out of arrays.
        if !self.is_type_copy(elem_type) {
            return Err(CompileError::new(
                ErrorKind::MoveOutOfIndex {
                    element_type: elem_type.name().to_string(),
                },
                span,
            )
            .with_help("use explicit methods like swap() or take() to remove elements"));
        }

        // Allocate a temporary slot for the computed array value
        let temp_slot = ctx.next_slot;
        let num_slots = self.abi_slot_count(base_type);
        ctx.next_slot += num_slots;

        // Emit StorageLive for the temporary
        let storage_live_ref = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot: temp_slot },
            ty: base_type,
            span,
        });

        // Emit Alloc to store the computed array
        let alloc_ref = air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot: temp_slot,
                init: base_result.air_ref,
            },
            ty: Type::UNIT,
            span,
        });

        // Create PlaceRead with Index projection on the temp slot
        let place_ref = air.make_place(
            AirPlaceBase::Local(temp_slot),
            std::iter::once(AirProjection::Index {
                array_type: base_type,
                index: index_result.air_ref,
            }),
        );
        let read_ref = air.add_inst(AirInst {
            data: AirInstData::PlaceRead { place: place_ref },
            ty: elem_type,
            span,
        });

        // Note: We don't emit StorageDead here. The temporary will be cleaned up by
        // scope-based drop elaboration in the CFG builder.
        let stmts_start = air.add_extra(&[storage_live_ref.as_u32(), alloc_ref.as_u32()]);
        let block_ref = air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start,
                stmts_len: 2,
                value: read_ref,
            },
            ty: elem_type,
            span,
        });
        Ok(AnalysisResult::new(block_ref, elem_type))
    }

    /// Analyze an array index write.
    ///
    /// This is a complex operation that handles VarRef and ParamRef bases.
    /// The full implementation is in analysis.rs as it's quite large.
    fn analyze_index_set(
        &mut self,
        air: &mut Air,
        base: InstRef,
        index: InstRef,
        value: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // ADR-0064: indexing on a `MutSlice(T)` value goes through a
        // dedicated runtime intrinsic. `Slice(T)[i] = v` is rejected
        // because reads-only.
        if let Some(result) =
            self.try_analyze_slice_index_write(air, base, index, value, span, ctx)?
        {
            return Ok(result);
        }
        // ADR-0066: indexing-write on Vec(T).
        if let Some(result) =
            self.try_analyze_vec_index_write(air, base, index, value, span, ctx)?
        {
            return Ok(result);
        }
        // Delegate to the main implementation in analysis.rs
        self.analyze_index_set_impl(air, base, index, value, span, ctx)
    }

    /// Peek at a base expression's resolved type without analysing it.
    ///
    /// Used by the slice-indexing fast paths so we don't accidentally emit
    /// AIR for a base whose array path will re-analyse it. Only handles
    /// the cases that can produce a slice value today: a local / parameter
    /// holding a slice, or a slice-typed function-call result.
    pub(crate) fn peek_inst_type(&self, inst_ref: InstRef, ctx: &AnalysisContext) -> Option<Type> {
        match &self.rir.get(inst_ref).data {
            InstData::VarRef { name } => {
                // `VarRef` resolves locals first, then falls through to
                // params (parallel to `analyze_var_ref` at line ~5105).
                // Without the param fallback, slice-typed method
                // parameters (e.g. `other: Slice(T)` in
                // `extend_from_slice`) would skip the slice-index fast
                // path and hit the array-only branch, which errors with
                // "cannot index into non-array type '<slice>'".
                ctx.locals.get(name).map(|l| l.ty).or_else(|| {
                    ctx.params.iter().find(|p| p.name == *name).map(|p| {
                        // Auto-deref Ref/MutRef params so a `borrow s:
                        // Slice(T)` peeks as `Slice(T)`, mirroring
                        // `analyze_var_ref`'s ty-derivation.
                        match p.ty.kind() {
                            crate::types::TypeKind::Ref(id) => self.type_pool.ref_def(id),
                            crate::types::TypeKind::MutRef(id) => self.type_pool.mut_ref_def(id),
                            _ => p.ty,
                        }
                    })
                })
            }
            InstData::ParamRef { name, .. } => {
                ctx.params.iter().find(|p| p.name == *name).map(|p| p.ty)
            }
            _ => None,
        }
    }

    /// ADR-0064: if `base` analyses to a `Slice(T)` / `MutSlice(T)` value,
    /// emit a runtime `slice_index_read` intrinsic. Otherwise return
    /// `Ok(None)` and let the array path handle it.
    fn try_analyze_slice_index_read(
        &mut self,
        air: &mut Air,
        base: InstRef,
        index: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Option<AnalysisResult>> {
        // Only take the slice path when the base's type is already known
        // to be a slice. Otherwise we'd double-analyse the base for the
        // array path, which can trigger spurious move-after-use errors.
        let peek_ty = self.peek_inst_type(base, ctx);
        if !matches!(
            peek_ty.map(|t| t.kind()),
            Some(TypeKind::Slice(_)) | Some(TypeKind::MutSlice(_))
        ) {
            return Ok(None);
        }
        let base_result = self.analyze_inst(air, base, ctx)?;
        let base_ty = base_result.ty;
        let elem_ty = match base_ty.kind() {
            TypeKind::Slice(id) => self.type_pool.slice_def(id),
            TypeKind::MutSlice(id) => self.type_pool.mut_slice_def(id),
            _ => {
                // Not a slice — defer to the array path. The analyzed AIR
                // for `base` is already cached in `air.cache` via
                // `analyze_inst`, so re-analysing in the place-tracing
                // path is safe and idempotent.
                return Ok(None);
            }
        };

        if !self.is_type_copy(elem_ty) {
            return Err(CompileError::new(
                ErrorKind::MoveOutOfIndex {
                    element_type: self.format_type_name(elem_ty),
                },
                span,
            ));
        }

        let index_result = self.analyze_inst(air, index, ctx)?;
        if !index_result.ty.is_integer()
            && !index_result.ty.is_error()
            && !index_result.ty.is_never()
        {
            return Err(CompileError::type_mismatch(
                "usize".to_string(),
                self.format_type_name(index_result.ty),
                self.rir.get(index).span,
            ));
        }

        let intrinsic_name = self.interner.get_or_intern("slice_index_read");
        let args_start =
            air.add_extra(&[base_result.air_ref.as_u32(), index_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: intrinsic_name,
                args_start,
                args_len: 2,
            },
            ty: elem_ty,
            span,
        });
        Ok(Some(AnalysisResult::new(air_ref, elem_ty)))
    }

    /// ADR-0064: if `base` analyses to a `MutSlice(T)` value, emit a
    /// runtime `slice_index_write` intrinsic. `Slice(T)` (immutable) base
    /// is rejected as a write target. Returns `Ok(None)` for non-slice
    /// bases so the array path handles them.
    fn try_analyze_slice_index_write(
        &mut self,
        air: &mut Air,
        base: InstRef,
        index: InstRef,
        value: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Option<AnalysisResult>> {
        let peek_ty = self.peek_inst_type(base, ctx);
        if !matches!(
            peek_ty.map(|t| t.kind()),
            Some(TypeKind::Slice(_)) | Some(TypeKind::MutSlice(_))
        ) {
            return Ok(None);
        }
        let base_result = self.analyze_inst(air, base, ctx)?;
        let base_ty = base_result.ty;
        let elem_ty = match base_ty.kind() {
            TypeKind::MutSlice(id) => self.type_pool.mut_slice_def(id),
            TypeKind::Slice(_) => {
                return Err(CompileError::new(
                    ErrorKind::AssignToImmutable(self.format_type_name(base_ty)),
                    span,
                ));
            }
            _ => return Ok(None),
        };

        let index_result = self.analyze_inst(air, index, ctx)?;
        if !index_result.ty.is_integer()
            && !index_result.ty.is_error()
            && !index_result.ty.is_never()
        {
            return Err(CompileError::type_mismatch(
                "usize".to_string(),
                self.format_type_name(index_result.ty),
                self.rir.get(index).span,
            ));
        }

        let value_result = self.analyze_inst(air, value, ctx)?;
        if value_result.ty != elem_ty && !value_result.ty.is_error() && !value_result.ty.is_never()
        {
            return Err(CompileError::type_mismatch(
                self.format_type_name(elem_ty),
                self.format_type_name(value_result.ty),
                self.rir.get(value).span,
            ));
        }

        let intrinsic_name = self.interner.get_or_intern("slice_index_write");
        let args_start = air.add_extra(&[
            base_result.air_ref.as_u32(),
            index_result.air_ref.as_u32(),
            value_result.air_ref.as_u32(),
        ]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: intrinsic_name,
                args_start,
                args_len: 3,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(Some(AnalysisResult::new(air_ref, Type::UNIT)))
    }

    // ========================================================================
    // Enum operations: EnumDecl, EnumVariant
    // ========================================================================

    /// Analyze an enum operation instruction.
    ///
    /// Handles: EnumDecl, EnumVariant
    pub(crate) fn analyze_enum_ops(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::EnumDecl { .. } => {
                // Enum declarations are processed during collection phase
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty: Type::UNIT,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::UNIT))
            }

            InstData::EnumVariant {
                module,
                type_name,
                variant,
            } => {
                // Look up the enum type, potentially through a module or comptime type variable
                let enum_id = if let Some(module_ref) = module {
                    // Qualified access: module.EnumName::Variant
                    self.resolve_enum_through_module(*module_ref, *type_name, inst.span)?
                } else if let Some(&enum_id) = self.enums.get(type_name) {
                    // Direct enum lookup
                    enum_id
                } else if let Some(&ty) = ctx.comptime_type_vars.get(type_name) {
                    // Comptime type variable (e.g., `let Opt = Option(i32); Opt::None`)
                    match ty.kind() {
                        TypeKind::Enum(id) => id,
                        _ => {
                            return Err(CompileError::new(
                                ErrorKind::UnknownEnumType(
                                    self.interner.resolve(type_name).to_string(),
                                ),
                                inst.span,
                            ));
                        }
                    }
                } else {
                    return Err(CompileError::new(
                        ErrorKind::UnknownEnumType(self.interner.resolve(type_name).to_string()),
                        inst.span,
                    ));
                };
                let enum_def = self.type_pool.enum_def(enum_id);

                // Find the variant index
                let variant_name = self.interner.resolve(variant);
                let variant_index = enum_def.find_variant(variant_name).ok_or_compile_error(
                    ErrorKind::UnknownVariant {
                        enum_name: enum_def.name.clone(),
                        variant_name: variant_name.to_string(),
                    },
                    inst.span,
                )?;

                let ty = Type::new_enum(enum_id);

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::EnumVariant {
                        enum_id,
                        variant_index: variant_index as u32,
                    },
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::EnumStructVariant {
                module,
                type_name,
                variant,
                fields_start,
                fields_len,
            } => self.analyze_enum_struct_variant(
                air,
                VariantRef {
                    module: *module,
                    type_name: *type_name,
                    variant: *variant,
                },
                (*fields_start, *fields_len),
                inst.span,
                ctx,
            ),

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_enum_ops called with non-enum instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }

    /// Analyze an enum struct variant construction: `Enum::Variant { field: value, ... }`
    fn analyze_enum_struct_variant(
        &mut self,
        air: &mut Air,
        variant_ref: VariantRef,
        (fields_start, fields_len): (u32, u32),
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let VariantRef {
            module,
            type_name,
            variant: variant_spur,
        } = variant_ref;
        // Look up the enum type, including comptime type variable resolution
        let enum_id = if let Some(module_ref) = module {
            self.resolve_enum_through_module(module_ref, type_name, span)?
        } else if let Some(&enum_id) = self.enums.get(&type_name) {
            enum_id
        } else if let Some(&ty) = ctx.comptime_type_vars.get(&type_name) {
            match ty.kind() {
                TypeKind::Enum(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::UnknownEnumType(self.interner.resolve(&type_name).to_string()),
                        span,
                    ));
                }
            }
        } else {
            return Err(CompileError::new(
                ErrorKind::UnknownEnumType(self.interner.resolve(&type_name).to_string()),
                span,
            ));
        };
        let enum_def = self.type_pool.enum_def(enum_id);
        let enum_name = enum_def.name.clone();

        // Find the variant
        let variant_name = self.interner.resolve(&variant_spur).to_string();
        let variant_index = enum_def.find_variant(&variant_name).ok_or_compile_error(
            ErrorKind::UnknownVariant {
                enum_name: enum_name.clone(),
                variant_name: variant_name.clone(),
            },
            span,
        )?;

        let variant_def = &enum_def.variants[variant_index];

        // Verify this is a struct variant
        if !variant_def.is_struct_variant() {
            return Err(CompileError::type_mismatch(
                format!(
                    "tuple-style construction for variant {}::{}",
                    enum_name, variant_name
                ),
                "struct-style construction { ... }".to_string(),
                span,
            ));
        }

        let field_types: Vec<Type> = variant_def.fields.clone();
        let field_names: Vec<String> = variant_def.field_names.clone();
        let qualified_name = format!("{}::{}", enum_name, variant_name);

        // Build field name to index map
        let field_index_map: rustc_hash::FxHashMap<&str, usize> = field_names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i))
            .collect();

        // Get field initializers
        let field_inits = self.rir.get_field_inits(fields_start, fields_len);

        // Check for unknown and duplicate fields
        let mut seen_fields = rustc_hash::FxHashSet::default();
        for (init_field_name, _) in &field_inits {
            let init_name = self.interner.resolve(init_field_name);

            if !field_index_map.contains_key(init_name) {
                return Err(CompileError::new(
                    ErrorKind::UnknownField {
                        struct_name: qualified_name.clone(),
                        field_name: init_name.to_string(),
                    },
                    span,
                ));
            }

            if !seen_fields.insert(init_name.to_string()) {
                return Err(CompileError::new(
                    ErrorKind::DuplicateField {
                        struct_name: qualified_name.clone(),
                        field_name: init_name.to_string(),
                    },
                    span,
                ));
            }
        }

        // Check all fields are provided
        if field_inits.len() != field_names.len() {
            let missing: Vec<String> = field_names
                .iter()
                .filter(|n| !seen_fields.contains(n.as_str()))
                .cloned()
                .collect();
            return Err(CompileError::new(
                ErrorKind::MissingFields(Box::new(MissingFieldsError {
                    struct_name: qualified_name,
                    missing_fields: missing,
                })),
                span,
            ));
        }

        // Analyze field values in source order, then reorder to declaration order
        let mut analyzed_fields: Vec<Option<AirRef>> = vec![None; field_names.len()];

        for (init_field_name, field_value) in &field_inits {
            let init_name = self.interner.resolve(init_field_name);
            let field_idx = field_index_map[init_name];
            let expected_type = field_types[field_idx];

            let result = self.analyze_inst(air, *field_value, ctx)?;

            if result.ty != expected_type {
                return Err(CompileError::type_mismatch(
                    expected_type.name().to_string(),
                    result.ty.name().to_string(),
                    span,
                )
                .with_label(
                    format!(
                        "field '{}' expects type {}",
                        init_name,
                        expected_type.name()
                    ),
                    span,
                ));
            }

            analyzed_fields[field_idx] = Some(result.air_ref);
        }

        // Collect in declaration order for EnumCreate
        let field_air_refs: Vec<u32> = analyzed_fields
            .into_iter()
            .map(|opt| opt.expect("all fields should be initialized").as_u32())
            .collect();

        let enum_type = Type::new_enum(enum_id);
        let air_fields_len = field_air_refs.len() as u32;
        let air_fields_start = air.add_extra(&field_air_refs);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::EnumCreate {
                enum_id,
                variant_index: variant_index as u32,
                fields_start: air_fields_start,
                fields_len: air_fields_len,
            },
            ty: enum_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, enum_type))
    }

    // ========================================================================
    // Call operations: Call, MethodCall, AssocFnCall
    // ========================================================================

    /// Analyze a call operation instruction.
    ///
    /// Handles: Call, MethodCall, AssocFnCall
    pub(crate) fn analyze_call_ops(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::Call {
                name,
                args_start,
                args_len,
            } => self.analyze_call(air, *name, *args_start, *args_len, inst.span, ctx),

            InstData::MethodCall {
                receiver,
                method,
                args_start,
                args_len,
            } => {
                let args = self.rir.get_call_args(*args_start, *args_len);
                self.analyze_method_call_impl(air, *receiver, *method, args, inst.span, ctx)
            }

            InstData::AssocFnCall {
                type_name,
                function,
                args_start,
                args_len,
            } => {
                let args = self.rir.get_call_args(*args_start, *args_len);
                self.analyze_assoc_fn_call_impl(air, *type_name, *function, args, inst.span, ctx)
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_call_ops called with non-call instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }

    /// Analyze a function call.
    fn analyze_call(
        &mut self,
        air: &mut Air,
        name: Spur,
        args_start: u32,
        args_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // ADR-0055 call-sugar: if `name` is a local variable (not a function)
        // and its type has a `__call` method, rewrite `f(args)` as
        // `f.__call(args)`. We do this *before* the function lookup so a
        // normal function with the same name takes precedence (today there is
        // no overlap because items and locals share a namespace, but keeping
        // the ordering explicit avoids surprises if that changes).
        if !self.functions.contains_key(&name)
            && let Some(sugar_result) =
                self.try_analyze_call_sugar(air, name, args_start, args_len, span, ctx)
        {
            return sugar_result;
        }

        // Look up the function
        let fn_name_str = self.interner.resolve(&name).to_string();
        let fn_info = self
            .functions
            .get(&name)
            .ok_or_compile_error(ErrorKind::UndefinedFunction(fn_name_str.clone()), span)?;

        // Check if calling an unchecked function requires a checked block
        if fn_info.is_unchecked && ctx.checked_depth == 0 {
            return Err(CompileError::new(
                ErrorKind::UncheckedCallRequiresChecked(fn_name_str),
                span,
            ));
        }

        // ADR-0078: if this entry is an item-level re-export alias
        // (`pub const X = mod.Y`), use the original symbol name `Y` from
        // here on so the emitted `Call` targets the actual function
        // definition, and so the lazy work queue tracks the right
        // identifier.
        //
        // ADR-0085: extern fns and `@mark(c)` exports may further
        // override the emitted symbol via `@link_name("…")`. Symbol
        // override takes precedence over alias canonicalization since
        // the override is what the linker sees.
        let name = fn_info.link_name.or(fn_info.canonical_name).unwrap_or(name);

        // Track this function as referenced (for lazy analysis)
        ctx.referenced_functions.insert(name);

        // Get parameter data from the arena
        let param_types = self.param_arena.types(fn_info.params);
        let param_modes = self.param_arena.modes(fn_info.params);
        let param_comptime = self.param_arena.comptime(fn_info.params);
        let param_names = self.param_arena.names(fn_info.params);

        let args = self.rir.get_call_args(args_start, args_len);
        // Check argument count
        if args.len() != param_types.len() {
            let expected = param_types.len();
            let found = args.len();
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount { expected, found },
                span,
            ));
        }

        // Check for exclusive access violation
        self.check_exclusive_access(&args, span)?;

        // Check that call-site argument modes match function parameter modes.
        // ADR-0076 Phase 2: a `&x` (`MakeRef { is_mut: false }`) expression
        // in argument position is accepted for a `Ref`-mode param, and a
        // `&mut x` (`MakeRef { is_mut: true }`) is accepted for a
        // `MutRef`-mode param. This composes the `Ref(I)` / `MutRef(I)`
        // surface form (which normalizes to the legacy `Ref` / `MutRef`
        // transport modes at interface signature time) with the new
        // construction syntax.
        for (i, (arg, expected_mode)) in args.iter().zip(param_modes.iter()).enumerate() {
            let arg_inst = &self.rir.get(arg.value).data;
            let arg_is_make_ref_immut =
                matches!(arg_inst, gruel_rir::InstData::MakeRef { is_mut: false, .. });
            let arg_is_make_ref_mut =
                matches!(arg_inst, gruel_rir::InstData::MakeRef { is_mut: true, .. });
            // ADR-0076 implicit forwarding: a bare param identifier (or
            // `&name` / `&mut name` over one) referring to a ref-typed
            // binding can satisfy a Ref/MutRef-mode callee param without
            // an explicit borrow keyword. Pairs with the runtime-arg
            // promotion further down.
            //
            // "ref-typed binding" covers both encodings the rest of sema
            // produces:
            //   (a) Mode-level: `mode = Ref | MutRef`. Anon-struct `self`
            //       receivers and `references_type_param`-deferred helper
            //       params use this — the source `Ref(T)` is squashed into
            //       the param mode.
            //   (b) Type-level: `type = Ref(_) | MutRef(_)`, `mode = Normal`.
            //       Regular `other: Ref(Self)` parameters and named-impl
            //       `self` receivers use this — the source `Ref(T)` is
            //       preserved as the concrete param type.
            // Without recognizing (b), `helper(Self, T, other)` where
            // `other: Ref(Self)` rejects the bare-name forward and demands
            // a `borrow` keyword, even though `other` already is the ref.
            let inner_var_name = match arg_inst {
                gruel_rir::InstData::VarRef { name } => Some(*name),
                gruel_rir::InstData::MakeRef { operand, .. } => {
                    match &self.rir.get(*operand).data {
                        gruel_rir::InstData::VarRef { name } => Some(*name),
                        _ => None,
                    }
                }
                _ => None,
            };
            let inner_param = inner_var_name.and_then(|n| ctx.params.iter().find(|p| p.name == n));
            let inner_param_mode = inner_param.map(|p| p.mode);
            let inner_param_ty_is_ref =
                inner_param.is_some_and(|p| matches!(p.ty.kind(), crate::types::TypeKind::Ref(_)));
            let inner_param_ty_is_mut_ref = inner_param
                .is_some_and(|p| matches!(p.ty.kind(), crate::types::TypeKind::MutRef(_)));
            let implicit_forward_ref = matches!(
                inner_param_mode,
                Some(RirParamMode::Ref) | Some(RirParamMode::MutRef)
            ) || inner_param_ty_is_ref
                || inner_param_ty_is_mut_ref;
            let implicit_forward_mut =
                matches!(inner_param_mode, Some(RirParamMode::MutRef)) || inner_param_ty_is_mut_ref;
            match expected_mode {
                RirParamMode::MutRef => {
                    if arg.mode != RirArgMode::MutRef
                        && !arg_is_make_ref_mut
                        && !implicit_forward_mut
                    {
                        return Err(CompileError::new(
                            ErrorKind::InoutKeywordMissing,
                            self.rir.get(args[i].value).span,
                        ));
                    }
                }
                RirParamMode::Ref => {
                    if arg.mode != RirArgMode::Ref
                        && !arg_is_make_ref_immut
                        && !implicit_forward_ref
                    {
                        return Err(CompileError::new(
                            ErrorKind::BorrowKeywordMissing,
                            self.rir.get(args[i].value).span,
                        ));
                    }
                }
                // Normal and comptime params accept any mode
                // (comptime params are substituted at compile time, not passed at runtime)
                RirParamMode::Normal | RirParamMode::Comptime => {
                    // Normal params accept any mode
                }
            }
        }

        // Extract info before any mutable borrow
        let is_generic = fn_info.is_generic;
        let param_types = param_types.to_vec();
        let param_comptime = param_comptime.to_vec();
        let param_names = param_names.to_vec();
        let param_modes_owned = param_modes.to_vec();
        let return_type_sym = fn_info.return_type_sym;
        let base_return_type = fn_info.return_type;
        let fn_body = fn_info.body;

        // Special case: functions that return `type` with only comptime parameters
        // should be evaluated at compile time.
        // This handles both:
        //   - `fn SimpleType() -> type { struct { x: i32 } }`  (no params)
        //   - `fn FixedBuffer(comptime N: i32) -> type { struct { fn capacity(self) -> i32 { N } } }`
        let all_params_comptime = param_comptime.iter().all(|&c| c);
        if base_return_type == Type::COMPTIME_TYPE && (args.is_empty() || all_params_comptime) {
            // Build value_subst from comptime VALUE parameters (e.g., comptime N: i32)
            let mut value_subst: rustc_hash::FxHashMap<Spur, ConstValue> =
                rustc_hash::FxHashMap::default();
            for (i, is_comptime) in param_comptime.iter().enumerate() {
                if *is_comptime && param_types[i] != Type::COMPTIME_TYPE {
                    // This is a comptime VALUE parameter - extract its const value.
                    // Use the full stateful interpreter so function calls work as comptime args.
                    if let Some(const_val) =
                        self.try_evaluate_comptime_arg(args[i].value, ctx, span)
                    {
                        value_subst.insert(param_names[i], const_val);
                    }
                }
            }
            // Try the simple constant-folding evaluator first — single-instruction
            // bodies (`fn F() -> type { struct {…} }`) hit this fast path.
            let empty_type_subst: rustc_hash::FxHashMap<Spur, Type> =
                rustc_hash::FxHashMap::default();
            let pending_eval_errs_before = self.pending_anon_eval_errors.len();
            // ADR-0082: track which type-constructor function is being
            // evaluated so the comptime path can populate the
            // `vec_instance_registry` when it processes the lang-item
            // Vec body's anonymous struct.
            let saved_ctor = self.comptime_ctor_fn.replace(name);
            let const_result =
                self.try_evaluate_const_with_subst(fn_body, &empty_type_subst, &value_subst);
            self.comptime_ctor_fn = saved_ctor;
            if let Some(ConstValue::Type(ty)) = const_result {
                // Success! Return a TypeConst instruction instead of a runtime call
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(ty),
                    ty: Type::COMPTIME_TYPE,
                    span,
                });
                return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
            }
            // The simple evaluator returned None. If it was because of a
            // specific anon-eval validation failure (empty struct, duplicate
            // method), surface that directly — running the rich-eval fallback
            // would just push the same error a second time.
            if self.pending_anon_eval_errors.len() > pending_eval_errs_before {
                let err = self
                    .pending_anon_eval_errors
                    .remove(pending_eval_errs_before);
                // Drop any duplicate entries pushed in the same simple-path call
                // so they don't surface twice at the end of analysis.
                self.pending_anon_eval_errors
                    .truncate(pending_eval_errs_before);
                return Err(err);
            }
            // Non-generic case (no type params): fall back to the rich comptime
            // interpreter for multi-statement bodies. For generic functions
            // (with `comptime T: type` params), let the dedicated generic path
            // below handle the fallback — it has the type substitution we need.
            if !is_generic {
                let saved_ctor = self.comptime_ctor_fn.replace(name);
                let ty_result = self.evaluate_type_ctor_body(
                    fn_body,
                    &empty_type_subst,
                    &value_subst,
                    ctx,
                    span,
                );
                self.comptime_ctor_fn = saved_ctor;
                let ty = ty_result?;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(ty),
                    ty: Type::COMPTIME_TYPE,
                    span,
                });
                return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
            }
            // Fall through to generic dispatch below.
        }

        // Check that comptime parameters receive compile-time constant values
        let has_comptime_params = param_comptime.iter().any(|&c| c);
        if has_comptime_params {
            // Validate each comptime parameter receives a compile-time constant
            for (i, (&is_comptime, arg)) in param_comptime.iter().zip(args.iter()).enumerate() {
                if is_comptime {
                    // Try to evaluate the argument at compile time (using the full
                    // stateful interpreter so function call results qualify as comptime).
                    let arg_span = self.rir.get(arg.value).span;
                    let is_comptime_known = self
                        .try_evaluate_comptime_arg(arg.value, ctx, arg_span)
                        .is_some()
                        || self.is_comptime_type_var(arg.value, ctx);
                    if !is_comptime_known {
                        let param_name = self.interner.resolve(&param_names[i]).to_string();
                        return Err(CompileError::new(
                            ErrorKind::ComptimeArgNotConst {
                                param_name: param_name.clone(),
                            },
                            self.rir.get(arg.value).span,
                        )
                        .with_help(format!(
                            "parameter '{}' is declared as 'comptime' and requires a compile-time known value",
                            param_name
                        )));
                    }
                }
            }
        }

        // ADR-0076: implicit forwarding of a `Ref(T)` / `MutRef(T)` parameter.
        // If the callee expects `MutRef(T)` / `Ref(T)` and the arg is a bare
        // `Var(name)` referencing a `Ref`/`MutRef`-mode parameter (legacy
        // transport for interface params) whose inner type is `T`, treat the
        // arg as if the user had written `&name` / `&mut name`. Re-mode the
        // arg here (before `analyze_call_args`) so the existing
        // borrow-tracking machinery sees it as a borrow and doesn't apply
        // move semantics; the post-process loop below also flips the AIR-arg
        // mode so codegen passes the param pointer. Also stash the binding
        // name in `ctx.borrow_arg_skip_move` for the duration of this arg's
        // analysis so a `Ref`-mode parameter read doesn't trigger a "move
        // out of borrow" error.
        let mut args: Vec<RirCallArg> = args.to_vec();
        let mut implicit_borrow_arg: Vec<Option<Spur>> = vec![None; args.len()];
        for (i, param_ty) in param_types.iter().enumerate() {
            // Two ways the callee param "wants a reference":
            //   (a) Static signature already resolved to `Ref(_)` /
            //       `MutRef(_)` — the original ADR-0076 case.
            //   (b) Generic helper whose source signature reads
            //       `Ref(VecT)` / `MutRef(VecT)` but whose stored
            //       param type is the `COMPTIME_TYPE` placeholder
            //       (because of `references_type_param` deferral in
            //       declarations.rs). Read the intended ref-shape off
            //       `param_modes[i]` instead — a `Ref` / `MutRef`
            //       *mode* set on a comptime-deferred param means the
            //       source wrote `Ref(...)` / `MutRef(...)`. The mode
            //       arrives via the regular pathway: parser →
            //       `resolve_param_type` (ADR-0076 Phase 2 normalization)
            //       → arena `modes` slot.
            let want_mut = match param_ty.kind() {
                crate::types::TypeKind::MutRef(_) => true,
                crate::types::TypeKind::Ref(_) => false,
                _ => {
                    if param_ty == &Type::COMPTIME_TYPE
                        && matches!(param_modes_owned[i], RirParamMode::MutRef)
                    {
                        true
                    } else if param_ty == &Type::COMPTIME_TYPE
                        && matches!(param_modes_owned[i], RirParamMode::Ref)
                    {
                        false
                    } else {
                        continue;
                    }
                }
            };
            let inner_ty_opt = match param_ty.kind() {
                crate::types::TypeKind::MutRef(id) => Some(self.type_pool.mut_ref_def(id)),
                crate::types::TypeKind::Ref(id) => Some(self.type_pool.ref_def(id)),
                _ => None,
            };
            if args[i].mode != gruel_rir::RirArgMode::Normal {
                continue;
            }
            if let InstData::VarRef { name } = &self.rir.get(args[i].value).data
                && let Some(p) = ctx.params.iter().find(|p| p.name == *name)
                && (
                    // Static case: param's resolved type matches the
                    // callee's resolved inner. The (b) case above
                    // doesn't have an inner_ty to compare against — the
                    // type-checker downstream will catch a real mismatch
                    // when the body is specialized.
                    inner_ty_opt.map(|t| t == p.ty).unwrap_or(true)
                )
            {
                // The caller's param is "ref-shaped" if its mode encodes
                // Ref/MutRef (legacy mode-level convention) OR its type
                // is Ref(_)/MutRef(_) (ADR-0076 type-level convention).
                // Either is enough for the implicit forward.
                let p_is_mut = matches!(p.mode, RirParamMode::MutRef)
                    || matches!(p.ty.kind(), crate::types::TypeKind::MutRef(_));
                let p_is_ref = matches!(p.mode, RirParamMode::Ref)
                    || matches!(p.ty.kind(), crate::types::TypeKind::Ref(_));
                let promoted_mode = match (want_mut, p_is_mut, p_is_ref) {
                    (true, true, _) => gruel_rir::RirArgMode::MutRef,
                    (false, _, true) => gruel_rir::RirArgMode::Ref,
                    (false, true, _) => {
                        // A `MutRef(T)` binding can be re-borrowed as `Ref(T)`
                        // (downgrading exclusivity to shared read-only).
                        gruel_rir::RirArgMode::Ref
                    }
                    _ => continue,
                };
                args[i].mode = promoted_mode;
                if p_is_ref && !p_is_mut {
                    implicit_borrow_arg[i] = Some(*name);
                }
            }
        }

        // Analyze all arguments. For each implicit-borrow arg we set
        // `ctx.borrow_arg_skip_move` so the param read inside the arg
        // doesn't fire the move-out check.
        let mut air_args: Vec<AirCallArg> = Vec::with_capacity(args.len());
        for (arg_idx, arg) in args.iter().enumerate() {
            let saved = ctx.borrow_arg_skip_move;
            ctx.borrow_arg_skip_move = implicit_borrow_arg[arg_idx];
            let air_arg = self
                .analyze_call_args(air, std::slice::from_ref(arg), ctx)
                .map(|mut v| v.remove(0));
            ctx.borrow_arg_skip_move = saved;
            air_args.push(air_arg?);
        }

        // ADR-0076: also flip the AIR-arg mode so codegen recognises the
        // implicit forward as by-pointer ABI.
        for (i, param_ty) in param_types.iter().enumerate() {
            let arg_air = air_args[i].value;
            let inner_ty = match param_ty.kind() {
                crate::types::TypeKind::MutRef(id) => Some(self.type_pool.mut_ref_def(id)),
                crate::types::TypeKind::Ref(id) => Some(self.type_pool.ref_def(id)),
                _ => None,
            };
            if let Some(inner_ty) = inner_ty
                && air.get(arg_air).ty == inner_ty
                && matches!(air.get(arg_air).data, AirInstData::Param { .. })
                && air_args[i].mode == AirArgMode::Normal
            {
                air_args[i].mode = match param_ty.kind() {
                    crate::types::TypeKind::MutRef(_) => AirArgMode::MutRef,
                    crate::types::TypeKind::Ref(_) => AirArgMode::Ref,
                    _ => unreachable!(),
                };
            }
        }

        // ADR-0056 Phase 4c/4d: for any argument whose corresponding
        // parameter has an interface type, run structural conformance
        // against the argument's concrete type and replace the argument
        // AIR with a `MakeInterfaceRef` coercion. Codegen (Phase 4d)
        // lowers that to a `(data_ptr, vtable_ptr)` fat-pointer struct.
        //
        // ADR-0076 Phase 2: with `Ref(I)` / `MutRef(I)` parameters, the
        // call-site arg may be a `&x` / `&mut x` expression whose AIR type
        // is `Ref(Concrete)` / `MutRef(Concrete)`. Unwrap the reference
        // before conformance and witness extraction so the same path
        // handles both legacy `borrow x` and new `&x` arg shapes.
        for (i, param_ty) in param_types.iter().enumerate() {
            if let crate::types::TypeKind::Interface(iface_id) = param_ty.kind() {
                let arg_air = air_args[i].value;
                let raw_arg_ty = air.get(arg_air).ty;
                let arg_ty = match raw_arg_ty.kind() {
                    crate::types::TypeKind::Ref(id) => self.type_pool.ref_def(id),
                    crate::types::TypeKind::MutRef(id) => self.type_pool.mut_ref_def(id),
                    _ => raw_arg_ty,
                };
                let arg_span = self.rir.get(args[i].value).span;
                let witness = self.check_conforms(arg_ty, iface_id, arg_span)?;
                let struct_id = match arg_ty.kind() {
                    crate::types::TypeKind::Struct(id) => id,
                    _ => {
                        return Err(CompileError::type_mismatch(
                            "type conforming to interface".to_string(),
                            arg_ty.name().to_string(),
                            arg_span,
                        ));
                    }
                };
                // Record the (struct, interface) pair plus the conformance
                // witness so codegen can emit a vtable.
                self.interface_vtables_needed
                    .entry((struct_id, iface_id))
                    .or_insert(witness.slot_methods);
                let coerced = air.add_inst(AirInst {
                    data: AirInstData::MakeInterfaceRef {
                        value: arg_air,
                        struct_id,
                        interface_id: iface_id,
                    },
                    ty: *param_ty,
                    span: arg_span,
                });
                air_args[i].value = coerced;
            }
        }

        // Handle generic function calls differently
        if is_generic {
            // Separate type/value comptime arguments from runtime arguments.
            let mut type_args: Vec<Type> = Vec::new();
            let mut value_args: Vec<ConstValue> = Vec::new();
            let mut runtime_args: Vec<AirCallArg> = Vec::new();
            let mut type_subst: rustc_hash::FxHashMap<Spur, Type> =
                rustc_hash::FxHashMap::default();

            for (i, (air_arg, is_comptime)) in
                air_args.iter().zip(param_comptime.iter()).enumerate()
            {
                if *is_comptime {
                    // Check if this is a type parameter (param type is ComptimeType)
                    // vs a value parameter (param type is i32, bool, etc.)
                    if param_types[i] == Type::COMPTIME_TYPE {
                        // This is a TYPE parameter - expect a TypeConst instruction
                        let inst = air.get(air_arg.value);
                        if let AirInstData::TypeConst(ty) = &inst.data {
                            type_args.push(*ty);
                            // Record the substitution: param_name -> concrete_type
                            type_subst.insert(param_names[i], *ty);
                        } else {
                            // Not a type - this is an error for type parameters
                            return Err(CompileError::new(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: "comptime type parameter must be a type literal"
                                        .to_string(),
                                },
                                span,
                            ));
                        }
                    } else {
                        // This is a VALUE parameter (e.g., `comptime n: i32`).
                        // Erase it: extract its compile-time constant for the
                        // specialization key and *don't* pass it as a runtime
                        // argument. The specialized body sees `n` as a comptime
                        // local bound to the captured value.
                        let arg_span = self.rir.get(args[i].value).span;
                        match self.try_evaluate_comptime_arg(args[i].value, ctx, arg_span) {
                            Some(const_val) => value_args.push(const_val),
                            None => {
                                let param_name = self.interner.resolve(&param_names[i]).to_string();
                                return Err(CompileError::new(
                                    ErrorKind::ComptimeArgNotConst {
                                        param_name: param_name.clone(),
                                    },
                                    arg_span,
                                )
                                .with_help(format!(
                                    "parameter '{}' is declared as 'comptime' and requires \
                                     a compile-time known value",
                                    param_name
                                )));
                            }
                        }
                    }
                } else {
                    runtime_args.push(air_arg.clone());
                }
            }

            // Determine the actual return type by substituting type parameters.
            // Three cases:
            //   1. Bare `-> T`: direct hit in `type_subst`.
            //   2. Compound `-> Ptr(T)` / `-> MutRef(Vec(T))`: the source-
            //      level return symbol isn't in `type_subst` directly, but
            //      the substituting resolver walks its inner positions.
            //   3. Concrete `-> i32`: both lookups miss; keep
            //      `base_return_type` unchanged.
            //
            // Without case 2, the AIR instruction emitted below would carry
            // `ty: COMPTIME_TYPE`, and the caller's `ret %call` would then
            // try to return a "type" value from a `Ptr(i32)`-typed function
            // — surfacing as an LLVM verifier failure instead of a sema
            // error.
            let return_type = if base_return_type == Type::COMPTIME_TYPE {
                if let Some(&concrete) = type_subst.get(&return_type_sym) {
                    concrete
                } else {
                    self.resolve_type_for_comptime_with_subst(return_type_sym, &type_subst)
                        .unwrap_or(base_return_type)
                }
            } else {
                base_return_type
            };

            // Special case: functions that return `type` (not a type parameter) with only comptime args
            // can be fully evaluated at compile time to produce a concrete anonymous struct type.
            // This handles cases like:
            //   - `fn Pair(comptime T: type) -> type { struct { first: T, second: T } }`
            //   - `fn FixedBuffer(comptime N: i32) -> type { struct { fn capacity(self) -> i32 { N } } }`
            let all_params_comptime = param_comptime.iter().all(|&c| c);
            if return_type == Type::COMPTIME_TYPE && all_params_comptime {
                // The return type is literally `type`, not a type parameter that was substituted.
                // Try to evaluate the function body at compile time with type substitutions.
                // Also build value_subst from comptime VALUE parameters (e.g., comptime N: i32)
                let mut value_subst: rustc_hash::FxHashMap<Spur, ConstValue> =
                    rustc_hash::FxHashMap::default();
                for (i, is_comptime) in param_comptime.iter().enumerate() {
                    if *is_comptime && param_types[i] != Type::COMPTIME_TYPE {
                        // This is a comptime VALUE parameter - extract its const value.
                        // Use the full stateful interpreter so function calls work as comptime args.
                        if let Some(const_val) =
                            self.try_evaluate_comptime_arg(args[i].value, ctx, span)
                        {
                            value_subst.insert(param_names[i], const_val);
                        }
                    }
                }
                let pending_eval_errs_before = self.pending_anon_eval_errors.len();
                // ADR-0082: ctor-fn tracking for vec_instance_registry.
                let saved_ctor = self.comptime_ctor_fn.replace(name);
                let const_result =
                    self.try_evaluate_const_with_subst(fn_body, &type_subst, &value_subst);
                self.comptime_ctor_fn = saved_ctor;
                if let Some(ConstValue::Type(ty)) = const_result {
                    // Success! Return a TypeConst instruction instead of a runtime call
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::TypeConst(ty),
                        ty: Type::COMPTIME_TYPE,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
                }
                // Surface specific anon-eval validation errors instead of
                // running the rich-eval fallback (which would just re-trigger
                // the same validation and push a duplicate error).
                if self.pending_anon_eval_errors.len() > pending_eval_errs_before {
                    let err = self
                        .pending_anon_eval_errors
                        .remove(pending_eval_errs_before);
                    self.pending_anon_eval_errors
                        .truncate(pending_eval_errs_before);
                    return Err(err);
                }
                // Fall back to the rich comptime interpreter for multi-statement
                // bodies (e.g. `comptime if @ownership(T) != Ownership::Copy {
                // @compile_error(...) }; struct {…}`). Errors propagate so the
                // user's `@compile_error` shows up as a real diagnostic.
                let saved_ctor = self.comptime_ctor_fn.replace(name);
                let ty_result =
                    self.evaluate_type_ctor_body(fn_body, &type_subst, &value_subst, ctx, span);
                self.comptime_ctor_fn = saved_ctor;
                let ty = ty_result?;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(ty),
                    ty: Type::COMPTIME_TYPE,
                    span,
                });
                return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
            }

            // Encode type arguments into extra array (as raw Type discriminants)
            let mut type_extra = Vec::with_capacity(type_args.len());
            for ty in &type_args {
                type_extra.push(ty.as_u32());
            }
            let type_args_start = air.add_extra(&type_extra);
            let type_args_len = type_args.len() as u32;

            // Encode runtime args into extra array
            let mut args_extra = Vec::with_capacity(runtime_args.len() * 2);
            for arg in &runtime_args {
                args_extra.push(arg.value.as_u32());
                args_extra.push(arg.mode.as_u32());
            }
            let runtime_args_start = air.add_extra(&args_extra);
            let runtime_args_len = runtime_args.len() as u32;

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::CallGeneric {
                    name,
                    type_args_start,
                    type_args_len,
                    args_start: runtime_args_start,
                    args_len: runtime_args_len,
                },
                ty: return_type,
                span,
            });
            // Record the comptime value arguments alongside the call so the
            // specialization pass can build a per-(name, type_args, value_args)
            // key. Without this, two calls that differ only by a `comptime n`
            // would collapse into the same specialization.
            if !value_args.is_empty() {
                air.set_comptime_value_args(air_ref.as_u32(), value_args);
            }
            Ok(AnalysisResult::new(air_ref, return_type))
        } else {
            // Regular non-generic call
            let return_type = base_return_type;

            // Encode call args into extra array
            let args_len = air_args.len() as u32;
            let mut extra_data = Vec::with_capacity(air_args.len() * 2);
            for arg in &air_args {
                extra_data.push(arg.value.as_u32());
                extra_data.push(arg.mode.as_u32());
            }
            let args_start = air.add_extra(&extra_data);

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Call {
                    name,
                    args_start,
                    args_len,
                },
                ty: return_type,
                span,
            });
            Ok(AnalysisResult::new(air_ref, return_type))
        }
    }

    // ========================================================================
    // Intrinsic operations: Intrinsic, TypeIntrinsic, TypeInterfaceIntrinsic
    // ========================================================================

    /// Analyze an intrinsic operation instruction.
    ///
    /// Handles: Intrinsic, TypeIntrinsic, TypeInterfaceIntrinsic
    pub(crate) fn analyze_intrinsic_ops(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::Intrinsic {
                name,
                args_start,
                args_len,
            } => {
                let arg_refs = self.rir.get_inst_refs(*args_start, *args_len);
                let args: Vec<RirCallArg> = arg_refs
                    .into_iter()
                    .map(|value| RirCallArg {
                        value,
                        mode: RirArgMode::Normal,
                    })
                    .collect();
                self.analyze_intrinsic_impl(air, inst_ref, *name, args, inst.span, ctx)
            }

            InstData::TypeIntrinsic { name, type_arg } => {
                self.analyze_type_intrinsic(air, *name, *type_arg, inst.span)
            }

            InstData::TypeInterfaceIntrinsic {
                name,
                type_arg,
                type_inst,
                interface_arg,
            } => self.analyze_type_interface_intrinsic(
                air,
                *name,
                *type_arg,
                *type_inst,
                *interface_arg,
                inst.span,
                ctx,
            ),

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_intrinsic_ops called with non-intrinsic instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }

    /// Analyze a type intrinsic (@size_of, @align_of, @type_name, @type_info, @ownership).
    fn analyze_type_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        type_arg: Spur,
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        let intrinsic_name = self.interner.resolve(&name);

        // @type_name and @type_info are comptime-only — reject in runtime context
        if intrinsic_name == "type_name" || intrinsic_name == "type_info" {
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!("@{intrinsic_name} can only be used inside a comptime block"),
                },
                span,
            ));
        }

        let ty = self.resolve_type(type_arg, span)?;

        match self.known.intrinsic_id(name) {
            Some(IntrinsicId::SizeOf) => {
                let slot_count = self.abi_slot_count(ty);
                let value = (slot_count * 8) as u64;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Const(value),
                    ty: Type::USIZE,
                    span,
                });
                Ok(AnalysisResult::new(air_ref, Type::USIZE))
            }
            Some(IntrinsicId::AlignOf) => {
                let slot_count = self.abi_slot_count(ty);
                let value = if slot_count == 0 { 1u64 } else { 8u64 };
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Const(value),
                    ty: Type::USIZE,
                    span,
                });
                Ok(AnalysisResult::new(air_ref, Type::USIZE))
            }
            Some(IntrinsicId::Ownership) => {
                let enum_id = self
                    .builtin_ownership_id
                    .expect("Ownership enum not injected - internal compiler error");
                let variant_index = self.ownership_variant_index(ty);
                let result_type = Type::new_enum(enum_id);
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::EnumVariant {
                        enum_id,
                        variant_index,
                    },
                    ty: result_type,
                    span,
                });
                Ok(AnalysisResult::new(air_ref, result_type))
            }
            Some(IntrinsicId::ThreadSafety) => {
                // ADR-0084: comptime classification on the trichotomy.
                // Stabilized in Phase 7 — no preview gate.
                let enum_id = self
                    .builtin_thread_safety_id
                    .expect("ThreadSafety enum not injected - internal compiler error");
                let variant_index = self.thread_safety_variant_index(ty);
                let result_type = Type::new_enum(enum_id);
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::EnumVariant {
                        enum_id,
                        variant_index,
                    },
                    ty: result_type,
                    span,
                });
                Ok(AnalysisResult::new(air_ref, result_type))
            }
            _ => Err(CompileError::new(
                ErrorKind::UnknownIntrinsic(intrinsic_name.to_string()),
                span,
            )),
        }
    }

    /// Analyze a type+interface intrinsic (`@implements(T, I)`).
    /// `type_inst`, when set, supersedes `type_arg` — sema
    /// comptime-evaluates it to a `ConstValue::Type`. Otherwise
    /// `type_arg` is treated as a type-name (or comptime-bound type
    /// alias) and resolved through `resolve_type` /
    /// `comptime_type_vars`.
    #[allow(clippy::too_many_arguments)]
    fn analyze_type_interface_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        type_arg: Spur,
        type_inst: Option<InstRef>,
        interface_arg: Spur,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let intrinsic_name = self.interner.resolve(&name);

        let id = match self.known.intrinsic_id(name) {
            Some(id) => id,
            None => {
                return Err(CompileError::new(
                    ErrorKind::UnknownIntrinsic(intrinsic_name.to_string()),
                    span,
                ));
            }
        };

        match id {
            IntrinsicId::Implements => {
                let ty = if let Some(t_inst) = type_inst {
                    // ADR-0079: comptime-evaluate the type
                    // expression. Use the heap-preserving evaluator
                    // so callers nested inside `comptime_unroll for`
                    // keep their loop binding intact.
                    use crate::sema::context::ConstValue;
                    let prev_steps = self.comptime_steps_used;
                    self.comptime_steps_used = 0;
                    let mut locals = ctx.comptime_value_vars.clone();
                    let val = self.evaluate_comptime_inst(t_inst, &mut locals, ctx, span)?;
                    self.comptime_steps_used = prev_steps;
                    match val {
                        ConstValue::Type(t) => t,
                        other => {
                            return Err(CompileError::new(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: format!(
                                        "@implements: type argument must comptime-evaluate to a type, got {:?}",
                                        other
                                    ),
                                },
                                span,
                            ));
                        }
                    }
                } else {
                    self.resolve_type(type_arg, span)?
                };
                let interface_id = match self.interfaces.get(&interface_arg).copied() {
                    Some(id) => id,
                    None => {
                        let iface_name = self.interner.resolve(&interface_arg).to_string();
                        return Err(CompileError::new(
                            ErrorKind::UnknownType(iface_name.clone()),
                            span,
                        )
                        .with_help(format!(
                            "`{iface_name}` is not an interface. The second argument to `@implements` must name an interface."
                        )));
                    }
                };
                let value: u64 = match self.check_conforms(ty, interface_id, span) {
                    Ok(_) => 1,
                    Err(_) => 0,
                };
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Const(value),
                    ty: Type::BOOL,
                    span,
                });
                Ok(AnalysisResult::new(air_ref, Type::BOOL))
            }
            _ => Err(CompileError::new(
                ErrorKind::UnknownIntrinsic(intrinsic_name.to_string()),
                span,
            )),
        }
    }

    // ========================================================================
    // Declaration no-ops: FnDecl
    // ========================================================================

    /// Analyze a declaration that produces Unit in expression context.
    pub(crate) fn analyze_decl_noop(
        &mut self,
        _air: &mut Air,
        inst_ref: InstRef,
        _ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::FnDecl { .. } => {
                // Function declarations are errors in expression context
                Err(CompileError::new(
                    ErrorKind::InternalError(
                        "FnDecl should not appear in expression context".to_string(),
                    ),
                    inst.span,
                ))
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_decl_noop called with non-declaration instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }
}

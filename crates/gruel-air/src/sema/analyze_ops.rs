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
//! - [`analyze_decl_noop`] - DropFnDecl (declarations that produce Unit)
//!
//! Binary operations (arithmetic, comparison, logical, bitwise) are handled
//! by existing helper methods in `analysis.rs`:
//! - `analyze_binary_arith` - Add, Sub, Mul, Div, Mod, BitAnd, BitOr, BitXor, Shl, Shr
//! - `analyze_comparison` - Eq, Ne, Lt, Gt, Le, Ge
//! - Logical And/Or are simple enough to remain inline

use std::collections::{HashMap, HashSet};

use gruel_error::{
    CompileError, CompileResult, CompileWarning, ErrorKind, MissingFieldsError, OptionExt,
    PreviewFeature, WarningKind,
};
use gruel_rir::{
    InstData, InstRef, RirArgMode, RirCallArg, RirParamMode, RirPattern, RirPatternBinding,
};
use lasso::Spur;

use crate::sema::context::ConstValue;
use gruel_span::Span;

use super::Sema;
use super::context::{AnalysisContext, AnalysisResult, LocalVar};
use crate::inst::{
    Air, AirCallArg, AirInst, AirInstData, AirPattern, AirPlaceBase, AirPlaceRef, AirProjection,
    AirRef,
};
use crate::scope::ScopedContext;
use crate::types::{Type, TypeKind};

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

impl PlaceTrace {
    /// Get the final type of the place (after all projections).
    pub fn result_type(&self) -> Type {
        self.projections
            .last()
            .map(|p| p.result_type)
            .unwrap_or(self.base_type)
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
                    return Ok(Some(PlaceTrace {
                        base: AirPlaceBase::Param(param_info.abi_slot),
                        base_type: param_info.ty,
                        projections: Vec::new(),
                        root_var: *name,
                        is_root_mutable: matches!(param_info.mode, RirParamMode::Inout),
                        is_borrow_param: matches!(param_info.mode, RirParamMode::Borrow),
                    }));
                }

                // Check if it's a local variable
                if let Some(local) = ctx.locals.get(name) {
                    return Ok(Some(PlaceTrace {
                        base: AirPlaceBase::Local(local.slot),
                        base_type: local.ty,
                        projections: Vec::new(),
                        root_var: *name,
                        is_root_mutable: local.is_mut,
                        is_borrow_param: false,
                    }));
                }

                // Not a variable - might be a constant or type name
                Ok(None)
            }

            // Base case: explicit parameter reference
            InstData::ParamRef { name, .. } => {
                if let Some(param_info) = ctx.params.iter().find(|p| p.name == *name) {
                    return Ok(Some(PlaceTrace {
                        base: AirPlaceBase::Param(param_info.abi_slot),
                        base_type: param_info.ty,
                        projections: Vec::new(),
                        root_var: *name,
                        is_root_mutable: matches!(param_info.mode, RirParamMode::Inout),
                        is_borrow_param: matches!(param_info.mode, RirParamMode::Borrow),
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
                        // Get the struct type from the base
                        let base_type = trace.result_type();
                        let struct_id = match base_type.as_struct() {
                            Some(id) => id,
                            None => {
                                // Module access or non-struct - not a place
                                return Ok(None);
                            }
                        };

                        // Look up field info
                        let struct_def = self.type_pool.struct_def(struct_id);
                        let field_name_str = self.interner.resolve(field);
                        let (field_index, struct_field) =
                            match struct_def.find_field(field_name_str) {
                                Some(info) => info,
                                None => return Ok(None), // Unknown field
                            };

                        let field_type = struct_field.ty;

                        // Add this projection with field name for move checking
                        trace.projections.push(ProjectionInfo {
                            proj: AirProjection::Field {
                                struct_id,
                                field_index: field_index as u32,
                            },
                            result_type: field_type,
                            field_name: Some(*field),
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
                        // Get the array type from the base
                        let base_type = trace.result_type();
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

            InstData::BoolConst(value) => {
                let ty = Type::BOOL;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::BoolConst(*value),
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

        match &inst.data {
            InstData::Neg { operand } => {
                // Get the resolved type from HM inference
                let ty = Self::get_resolved_type(ctx, inst_ref, inst.span, "negation operator")?;

                // Check if trying to negate an unsigned type.
                if ty.is_unsigned() {
                    return Err(CompileError::new(
                        ErrorKind::CannotNegateUnsigned(ty.name().to_string()),
                        inst.span,
                    )
                    .with_note("unsigned values cannot be negated"));
                }

                // Special case: negating a literal that equals |MIN| for signed types.
                let operand_inst = self.rir.get(*operand);
                if let InstData::IntConst(value) = &operand_inst.data {
                    // Check if this value, when negated, fits in the target signed type
                    if ty.negated_literal_fits(*value) && !ty.literal_fits(*value) {
                        // This is the MIN value case - store the MIN value directly.
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
                }

                let operand_result = self.analyze_inst(air, *operand, ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Neg(operand_result.air_ref),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::Not { operand } => {
                let operand_result = self.analyze_inst(air, *operand, ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Not(operand_result.air_ref),
                    ty: Type::BOOL,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::BOOL))
            }

            InstData::BitNot { operand } => {
                // Get the resolved type from HM inference
                let ty = Self::get_resolved_type(ctx, inst_ref, inst.span, "bitwise NOT operator")?;

                // Bitwise NOT operates on integer types only
                if !ty.is_integer() && !ty.is_error() && !ty.is_never() {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: "integer type".to_string(),
                            found: ty.name().to_string(),
                        },
                        inst.span,
                    ));
                }

                let operand_result = self.analyze_inst(air, *operand, ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::BitNot(operand_result.air_ref),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_unary_op called with non-unary instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
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

        match &inst.data {
            InstData::And { lhs, rhs } => {
                let lhs_result = self.analyze_inst(air, *lhs, ctx)?;
                let rhs_result = self.analyze_inst(air, *rhs, ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::And(lhs_result.air_ref, rhs_result.air_ref),
                    ty: Type::BOOL,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::BOOL))
            }

            InstData::Or { lhs, rhs } => {
                let lhs_result = self.analyze_inst(air, *lhs, ctx)?;
                let rhs_result = self.analyze_inst(air, *rhs, ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Or(lhs_result.air_ref, rhs_result.air_ref),
                    ty: Type::BOOL,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::BOOL))
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_logical_op called with non-logical instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
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
            } => self.analyze_branch(air, *cond, *then_block, *else_block, inst.span, ctx),

            InstData::Loop { cond, body } => {
                self.analyze_while_loop(air, *cond, *body, inst.span, ctx)
            }

            InstData::For {
                binding,
                is_mut,
                iterable,
                body,
            } => self.analyze_for_loop(air, *binding, *is_mut, *iterable, *body, inst.span, ctx),

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

    /// Analyze a branch (if-else) expression.
    fn analyze_branch(
        &mut self,
        air: &mut Air,
        cond: InstRef,
        then_block: InstRef,
        else_block: Option<InstRef>,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
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
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: then_type.name().to_string(),
                                found: else_type.name().to_string(),
                            },
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
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "()".to_string(),
                        found: then_type.name().to_string(),
                    },
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
        binding: Spur,
        is_mut: bool,
        iterable: InstRef,
        body: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Gate behind preview feature
        self.require_preview(PreviewFeature::ForLoops, "for-in loops", span)?;

        // Check if the iterable is a @range(...) intrinsic
        let iterable_inst = self.rir.get(iterable);
        match &iterable_inst.data {
            InstData::Intrinsic {
                name,
                args_start,
                args_len,
            } if *name == self.known.range => {
                let args = self.rir.get_inst_refs(*args_start, *args_len);
                self.analyze_range_for_loop(air, binding, is_mut, &args, body, span, iterable_inst.span, ctx)
            }
            _ => {
                // Not @range — analyze the iterable expression and check if it's an array
                let iterable_span = iterable_inst.span;
                let iterable_result = self.analyze_inst(air, iterable, ctx)?;
                let iterable_type = iterable_result.ty;

                if let Some(array_type_id) = iterable_type.as_array() {
                    let (elem_type, array_len) = self.type_pool.array_def(array_type_id);

                    if !self.is_type_copy(elem_type) {
                        return Err(CompileError::new(
                            ErrorKind::MoveOutOfIndex {
                                element_type: elem_type.name().to_string(),
                            },
                            iterable_span,
                        )
                        .with_help("for-in loops over arrays with non-Copy element types are not yet supported"));
                    }

                    self.analyze_array_for_loop(
                        air, binding, is_mut, iterable_result.air_ref, iterable_type,
                        elem_type, array_len, body, span, ctx,
                    )
                } else {
                    Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: "array or @range(...)".to_string(),
                            found: iterable_type.name().to_string(),
                        },
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
        binding: Spur,
        is_mut: bool,
        range_args: &[InstRef],
        body: InstRef,
        span: Span,
        range_span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
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
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: format!("{}", iter_type),
                },
                range_span,
            ));
        }

        // Analyze start (default: 0)
        let start_air = if let Some(start_ref) = start_ref {
            let result = self.analyze_inst(air, start_ref, ctx)?;
            if result.ty != iter_type {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: format!("{}", iter_type),
                        found: format!("{}", result.ty),
                    },
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
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: format!("{}", iter_type),
                        found: format!("{}", result.ty),
                    },
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
            data: AirInstData::Lt(counter_load_for_cond, end_result.air_ref),
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
            data: AirInstData::Add(counter_load_for_inc, stride_air),
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
        let outer_stmts_start = air.add_extra(&[
            counter_storage_live.as_u32(),
            counter_alloc.as_u32(),
        ]);
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
    /// Only supports Copy element types (Phase 3). The array is spilled to a
    /// temporary slot and indexed with a counter variable.
    fn analyze_array_for_loop(
        &mut self,
        air: &mut Air,
        binding: Spur,
        is_mut: bool,
        arr_air: AirRef,
        arr_type: Type,
        elem_type: Type,
        array_len: u64,
        body: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
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
            data: AirInstData::Lt(counter_load_for_cond, len_const),
            ty: Type::BOOL,
            span,
        });

        // Build loop body
        ctx.push_scope();
        ctx.loop_depth += 1;

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
            data: AirInstData::Add(counter_load_for_inc, one_const),
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

        // Validate that we can match on this type (integers, booleans, and enums)
        if !scrutinee_type.is_integer() && scrutinee_type != Type::BOOL && !scrutinee_type.is_enum()
        {
            return Err(CompileError::new(
                ErrorKind::InvalidMatchType(scrutinee_type.name().to_string()),
                span,
            ));
        }

        let arms = self.rir.get_match_arms(arms_start, arms_len);
        // Check for empty match
        if arms.is_empty() {
            return Err(CompileError::new(ErrorKind::EmptyMatch, span));
        }

        // Track patterns for exhaustiveness checking and duplicate detection
        let mut wildcard_span: Option<Span> = None;
        let mut bool_true_span: Option<Span> = None;
        let mut bool_false_span: Option<Span> = None;
        let mut seen_ints: HashMap<i64, Span> = HashMap::new();
        let mut covered_variants: HashSet<u32> = HashSet::new();
        let mut pattern_enum_id: Option<crate::types::EnumId> = None;

        // Analyze each arm (each arm gets its own scope)
        let mut air_arms = Vec::new();
        let mut result_type: Option<Type> = None;

        for (pattern, body) in arms.iter() {
            let pattern_span = pattern.span();

            // Cached resolution for enum patterns (set during validation, reused
            // in body analysis and AIR pattern conversion to avoid repeated lookups).
            let mut resolved_enum: Option<(crate::types::EnumId, u32)> = None;

            // If we've seen a wildcard, everything after is unreachable
            if let Some(first_wildcard_span) = wildcard_span {
                let pat_str = match pattern {
                    RirPattern::Wildcard(_) => "_".to_string(),
                    RirPattern::Int(n, _) => n.to_string(),
                    RirPattern::Bool(b, _) => b.to_string(),
                    RirPattern::Path {
                        type_name, variant, ..
                    }
                    | RirPattern::DataVariant {
                        type_name, variant, ..
                    }
                    | RirPattern::StructVariant {
                        type_name, variant, ..
                    } => {
                        format!(
                            "{}::{}",
                            self.interner.resolve(type_name),
                            self.interner.resolve(variant)
                        )
                    }
                };
                ctx.warnings.push(
                    CompileWarning::new(
                        WarningKind::UnreachablePattern(pat_str),
                        pattern_span,
                    )
                    .with_label("previous wildcard pattern here", first_wildcard_span)
                    .with_note(
                        "this pattern will never be matched because the wildcard pattern above matches everything",
                    ),
                );
            }

            // Validate pattern against scrutinee type and check for duplicates
            match pattern {
                RirPattern::Wildcard(_) => {
                    if wildcard_span.is_none() {
                        wildcard_span = Some(pattern_span);
                    }
                }
                RirPattern::Int(n, _) => {
                    if !scrutinee_type.is_integer() {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: scrutinee_type.name().to_string(),
                                found: "integer".to_string(),
                            },
                            pattern_span,
                        ));
                    }
                    // Check for duplicate integer pattern
                    if let Some(first_span) = seen_ints.get(n) {
                        if wildcard_span.is_none() {
                            ctx.warnings.push(
                                CompileWarning::new(
                                    WarningKind::UnreachablePattern(n.to_string()),
                                    pattern_span,
                                )
                                .with_label("first occurrence of this pattern", *first_span)
                                .with_note(
                                    "this pattern will never be matched because an earlier arm already matches the same value",
                                ),
                            );
                        }
                    } else {
                        seen_ints.insert(*n, pattern_span);
                    }
                }
                RirPattern::Bool(b, _) => {
                    if scrutinee_type != Type::BOOL {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: scrutinee_type.name().to_string(),
                                found: "bool".to_string(),
                            },
                            pattern_span,
                        ));
                    }
                    // Check for duplicate boolean pattern
                    let (first_span_opt, is_true) = if *b {
                        (&mut bool_true_span, true)
                    } else {
                        (&mut bool_false_span, false)
                    };
                    if let Some(first_span) = *first_span_opt {
                        if wildcard_span.is_none() {
                            ctx.warnings.push(
                                CompileWarning::new(
                                    WarningKind::UnreachablePattern(is_true.to_string()),
                                    pattern_span,
                                )
                                .with_label("first occurrence of this pattern", first_span)
                                .with_note(
                                    "this pattern will never be matched because an earlier arm already matches the same value",
                                ),
                            );
                        }
                    } else {
                        *first_span_opt = Some(pattern_span);
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
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: scrutinee_type.name().to_string(),
                                found: enum_def.name.clone(),
                            },
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

                    covered_variants.insert(variant_index as u32);
                    pattern_enum_id = Some(enum_id);
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
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: scrutinee_type.name().to_string(),
                                found: enum_def.name.clone(),
                            },
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

                    // Check binding count matches field count
                    let field_count = enum_def.variants[variant_index].fields.len();
                    if bindings.len() != field_count {
                        return Err(CompileError::new(
                            ErrorKind::WrongArgumentCount {
                                expected: field_count,
                                found: bindings.len(),
                            },
                            pattern_span,
                        ));
                    }

                    covered_variants.insert(variant_index as u32);
                    pattern_enum_id = Some(enum_id);
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
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: scrutinee_type.name().to_string(),
                                found: enum_def.name.clone(),
                            },
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
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: format!(
                                    "tuple-style pattern `{}::{}(...)`",
                                    enum_def.name, variant_name
                                ),
                                found: format!(
                                    "struct-style pattern `{}::{} {{ ... }}`",
                                    enum_def.name, variant_name
                                ),
                            },
                            pattern_span,
                        ));
                    }

                    // Check for unknown, duplicate, and missing fields
                    let mut seen_fields =
                        std::collections::HashSet::with_capacity(field_bindings.len());
                    let qualified_name = format!("{}::{}", enum_def.name, variant_name);
                    for fb in field_bindings {
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

                    // Check for missing fields
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

                    covered_variants.insert(variant_index as u32);
                    pattern_enum_id = Some(enum_id);
                    resolved_enum = Some((enum_id, variant_index as u32));
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
            let indexed_bindings: Option<Vec<(usize, &RirPatternBinding)>> = match pattern {
                RirPattern::DataVariant { bindings, .. } => {
                    Some(bindings.iter().enumerate().collect())
                }
                RirPattern::StructVariant { field_bindings, .. } => {
                    let (enum_id, variant_index) = resolved_enum
                        .expect("resolved_enum must be set for StructVariant patterns");
                    let enum_def = self.type_pool.enum_def(enum_id);
                    let variant_def = &enum_def.variants[variant_index as usize];
                    Some(
                        field_bindings
                            .iter()
                            .map(|fb| {
                                let field_name = self.interner.resolve(&fb.field_name);
                                let idx = variant_def
                                    .find_field(field_name)
                                    .expect("field name validated during pattern checking");
                                (idx, &fb.binding)
                            })
                            .collect(),
                    )
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
                let inner_result = self.analyze_inst(air, *body, ctx)?;
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
            } else {
                // Non-data/struct variant: analyze body normally
                self.analyze_inst(air, *body, ctx)?
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
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: prev.name().to_string(),
                                found: body_type.name().to_string(),
                            },
                            self.rir.get(*body).span,
                        ));
                    } else {
                        prev
                    }
                }
            });

            // Convert pattern to AIR pattern. For enum patterns (Path and DataVariant),
            // reuse the enum_id and variant_index resolved during validation.
            let air_pattern = match pattern {
                RirPattern::Wildcard(_) => AirPattern::Wildcard,
                RirPattern::Int(n, _) => AirPattern::Int(*n),
                RirPattern::Bool(b, _) => AirPattern::Bool(*b),
                RirPattern::Path { .. }
                | RirPattern::DataVariant { .. }
                | RirPattern::StructVariant { .. } => {
                    let (enum_id, variant_index) =
                        resolved_enum.expect("resolved_enum must be set for enum patterns");
                    AirPattern::EnumVariant {
                        enum_id,
                        variant_index,
                    }
                }
            };

            air_arms.push((air_pattern, body_result.air_ref));
        }

        // Exhaustiveness checking
        let has_wildcard = wildcard_span.is_some();
        let bool_true_covered = bool_true_span.is_some();
        let bool_false_covered = bool_false_span.is_some();
        let is_exhaustive = if scrutinee_type == Type::BOOL {
            has_wildcard || (bool_true_covered && bool_false_covered)
        } else if let Some(enum_id) = pattern_enum_id {
            let enum_def = self.type_pool.enum_def(enum_id);
            has_wildcard || covered_variants.len() == enum_def.variant_count()
        } else {
            // For integers, must have wildcard
            has_wildcard
        };

        if !is_exhaustive {
            return Err(CompileError::new(ErrorKind::NonExhaustiveMatch, span));
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

            // Type check: returned value must match function's return type.
            if !ctx.return_type.is_error()
                && !inner_ty.is_error()
                && !inner_ty.can_coerce_to(&ctx.return_type)
            {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: ctx.return_type.name().to_string(),
                        found: inner_ty.name().to_string(),
                    },
                    span,
                ));
            }
            Some(inner_result.air_ref)
        } else {
            // `return;` without expression - only valid for unit-returning functions
            if ctx.return_type != Type::UNIT && !ctx.return_type.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: ctx.return_type.name().to_string(),
                        found: "()".to_string(),
                    },
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

            InstData::VarRef { name } => self.analyze_var_ref(air, *name, inst.span, ctx),

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
                init,
                ..
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
        let struct_id = init_type.as_struct().ok_or_else(|| {
            CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: type_name_str.clone(),
                    found: init_type.name().to_string(),
                },
                span,
            )
        })?;

        let struct_def = self.type_pool.struct_def(struct_id);

        // Validate the type name matches
        if struct_def.name != type_name_str {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: type_name_str,
                    found: struct_def.name.clone(),
                },
                span,
            ));
        }

        // Get the destructure fields from the RIR
        let rir_fields = self.rir.get_destructure_fields(fields_start, fields_len);

        // Validate: no duplicate fields
        let mut seen_fields = std::collections::HashSet::new();
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

        // Validate: all struct fields are mentioned
        let struct_field_names: Vec<String> =
            struct_def.fields.iter().map(|f| f.name.clone()).collect();
        for struct_field_name in &struct_field_names {
            if !seen_fields.contains(struct_field_name) {
                return Err(CompileError::new(
                    ErrorKind::MissingFieldInDestructure {
                        struct_name: type_name_str.clone(),
                        field: struct_field_name.clone(),
                    },
                    span,
                ));
            }
        }

        // Validate: no unknown fields
        for field in &rir_fields {
            let field_name = self.interner.resolve(&field.field_name).to_string();
            if struct_def.find_field(&field_name).is_none() {
                return Err(CompileError::new(
                    ErrorKind::UnknownField {
                        struct_name: type_name_str.clone(),
                        field_name,
                    },
                    span,
                ));
            }
        }

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
        name: Spur,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // First check if it's a parameter
        if let Some(param_info) = ctx.params.iter().find(|p| p.name == name) {
            let ty = param_info.ty;
            let name_str = self.interner.resolve(&name);

            // Check if this parameter has been moved
            if let Some(move_state) = ctx.moved_vars.get(&name)
                && let Some(moved_span) = move_state.is_any_part_moved()
            {
                return Err(
                    CompileError::new(ErrorKind::UseAfterMove(name_str.to_string()), span)
                        .with_label("value moved here", moved_span),
                );
            }

            // Handle move semantics based on parameter mode
            if !self.is_type_copy(ty) {
                match param_info.mode {
                    // Normal and comptime parameters behave similarly for moves
                    // (comptime params are substituted at compile time)
                    RirParamMode::Normal | RirParamMode::Comptime => {
                        ctx.moved_vars
                            .entry(name)
                            .or_default()
                            .mark_path_moved(&[], span);
                    }
                    RirParamMode::Inout => {
                        ctx.moved_vars
                            .entry(name)
                            .or_default()
                            .mark_path_moved(&[], span);
                    }
                    RirParamMode::Borrow => {
                        let name_str = self.interner.resolve(&name);
                        return Err(CompileError::new(
                            ErrorKind::MoveOutOfBorrow {
                                variable: name_str.to_string(),
                            },
                            span,
                        ));
                    }
                }
            }

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Param {
                    index: param_info.abi_slot,
                },
                ty,
                span,
            });
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
                return Err(
                    CompileError::new(ErrorKind::UseAfterMove(name_str.to_string()), span)
                        .with_label("value moved here", moved_span),
                );
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
                    // For now, emit as i32 const. TODO: Track actual type.
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Const(*val as u64),
                        ty: Type::I32,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, Type::I32));
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
                ConstValue::Unit
                | ConstValue::Struct(_)
                | ConstValue::Array(_)
                | ConstValue::EnumVariant { .. }
                | ConstValue::EnumData { .. }
                | ConstValue::EnumStruct { .. }
                | ConstValue::BreakSignal
                | ConstValue::ContinueSignal
                | ConstValue::ReturnSignal => {
                    unreachable!("control-flow signal or composite value in comptime_value_vars")
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

        let ty = param_info.ty;

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
            // Check parameter mode - only inout can be assigned to
            match param_info.mode {
                // Normal and comptime parameters are immutable
                RirParamMode::Normal | RirParamMode::Comptime => {
                    return Err(CompileError::new(
                        ErrorKind::AssignToImmutable(name_str.to_string()),
                        span,
                    )
                    .with_help(format!(
                        "consider making parameter `{}` inout: `inout {}: {}`",
                        name_str,
                        name_str,
                        param_info.ty.name()
                    )));
                }
                RirParamMode::Inout => {
                    // Inout parameters can be assigned to
                }
                RirParamMode::Borrow => {
                    return Err(CompileError::new(
                        ErrorKind::MutateBorrowedValue {
                            variable: name_str.to_string(),
                        },
                        span,
                    ));
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

        // Check mutability
        if !local.is_mut {
            return Err(CompileError::new(
                ErrorKind::AssignToImmutable(name_str.to_string()),
                span,
            )
            .with_label("variable declared as immutable here", local.span)
            .with_help(format!(
                "consider making `{}` mutable: `let mut {}`",
                name_str, name_str
            )));
        }

        let slot = local.slot;

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
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: "struct type".to_string(),
                            found: ty.name().to_string(),
                        },
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
        let field_index_map: std::collections::HashMap<&str, usize> = struct_def
            .fields
            .iter()
            .enumerate()
            .map(|(i, f)| (f.name.as_str(), i))
            .collect();

        // Check for unknown or duplicate fields
        let mut seen_fields = std::collections::HashSet::new();
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
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: expected_field_type.name().to_string(),
                        found: field_result.ty.name().to_string(),
                    },
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
                return Err(CompileError::new(
                    ErrorKind::UseAfterMove(root_name.to_string()),
                    span,
                )
                .with_label("value moved here", moved_span));
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
                .map(|id| self.type_pool.struct_def(id).is_linear)
                .unwrap_or(false);

            // Move checking using the trace
            if is_linear {
                // For linear types, field access consumes the entire struct
                ctx.moved_vars
                    .entry(trace.root_var)
                    .or_default()
                    .mark_path_moved(&[], span);
            } else if !self.is_type_copy(field_type) {
                // ADR-0036: Ban partial field moves. Must destructure instead.
                let type_name = parent_type
                    .as_struct()
                    .map(|id| self.type_pool.struct_def(id).name.clone())
                    .unwrap_or_else(|| parent_type.name().to_string());
                let field_name = self.interner.resolve(&field).to_string();
                return Err(CompileError::new(
                    ErrorKind::CannotMoveField {
                        type_name: type_name.clone(),
                        field: field_name.clone(),
                    },
                    span,
                )
                .with_help(format!(
                    "use destructuring: `let {type_name} {{ {field_name}, .. }} = ...;`"
                )));
            }

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

        let (field_index, struct_field) =
            struct_def.find_field(&field_name_str).ok_or_compile_error(
                ErrorKind::UnknownField {
                    struct_name: struct_def.name.clone(),
                    field_name: field_name_str.clone(),
                },
                span,
            )?;

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
        // Delegate to the main implementation in analysis.rs
        self.analyze_index_set_impl(air, base, index, value, span, ctx)
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
                *module,
                *type_name,
                *variant,
                *fields_start,
                *fields_len,
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
    #[allow(clippy::too_many_arguments)]
    fn analyze_enum_struct_variant(
        &mut self,
        air: &mut Air,
        module: Option<InstRef>,
        type_name: Spur,
        variant_spur: Spur,
        fields_start: u32,
        fields_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
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
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: format!(
                        "tuple-style construction for variant {}::{}",
                        enum_name, variant_name
                    ),
                    found: "struct-style construction { ... }".to_string(),
                },
                span,
            ));
        }

        let field_types: Vec<Type> = variant_def.fields.clone();
        let field_names: Vec<String> = variant_def.field_names.clone();
        let qualified_name = format!("{}::{}", enum_name, variant_name);

        // Build field name to index map
        let field_index_map: std::collections::HashMap<&str, usize> = field_names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i))
            .collect();

        // Get field initializers
        let field_inits = self.rir.get_field_inits(fields_start, fields_len);

        // Check for unknown and duplicate fields
        let mut seen_fields = std::collections::HashSet::new();
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
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: expected_type.name().to_string(),
                        found: result.ty.name().to_string(),
                    },
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

        // Check that call-site argument modes match function parameter modes
        // Do this before the mutable borrow in analyze_call_args, accessing fn_info directly
        for (i, (arg, expected_mode)) in args.iter().zip(param_modes.iter()).enumerate() {
            match expected_mode {
                RirParamMode::Inout => {
                    if arg.mode != RirArgMode::Inout {
                        return Err(CompileError::new(
                            ErrorKind::InoutKeywordMissing,
                            self.rir.get(args[i].value).span,
                        ));
                    }
                }
                RirParamMode::Borrow => {
                    if arg.mode != RirArgMode::Borrow {
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
            let mut value_subst: std::collections::HashMap<Spur, ConstValue> =
                std::collections::HashMap::new();
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
            // Try to evaluate the function body at compile time
            if let Some(ConstValue::Type(ty)) = self.try_evaluate_const_with_subst(
                fn_body,
                &std::collections::HashMap::new(),
                &value_subst,
            ) {
                // Success! Return a TypeConst instruction instead of a runtime call
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(ty),
                    ty: Type::COMPTIME_TYPE,
                    span,
                });
                return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
            }
            // If we can't evaluate at compile time, fall through to runtime call
            // (which will fail at link time, but gives a better error experience)
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

        // Analyze all arguments
        let air_args = self.analyze_call_args(air, &args, ctx)?;

        // Handle generic function calls differently
        if is_generic {
            // Separate type arguments from runtime arguments
            let mut type_args: Vec<Type> = Vec::new();
            let mut runtime_args: Vec<AirCallArg> = Vec::new();
            let mut type_subst: std::collections::HashMap<Spur, Type> =
                std::collections::HashMap::new();

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
                        // This is a VALUE parameter (e.g., comptime n: i32)
                        // It's still passed at runtime but must be a compile-time constant.
                        // The constant-ness has already been validated above.
                        // We don't erase value parameters - they're passed normally.
                        runtime_args.push(air_arg.clone());
                    }
                } else {
                    runtime_args.push(air_arg.clone());
                }
            }

            // Determine the actual return type by substituting type parameters
            let return_type = if base_return_type == Type::COMPTIME_TYPE {
                // Return type is a type parameter - look it up in substitutions
                *type_subst
                    .get(&return_type_sym)
                    .unwrap_or(&base_return_type)
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
                let mut value_subst: std::collections::HashMap<Spur, ConstValue> =
                    std::collections::HashMap::new();
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
                if let Some(ConstValue::Type(ty)) =
                    self.try_evaluate_const_with_subst(fn_body, &type_subst, &value_subst)
                {
                    // Success! Return a TypeConst instruction instead of a runtime call
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::TypeConst(ty),
                        ty: Type::COMPTIME_TYPE,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
                }
                // If we can't evaluate at compile time, fall through to the error below
                // (we can't have a runtime call that returns `type`)
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
    // Intrinsic operations: Intrinsic, TypeIntrinsic
    // ========================================================================

    /// Analyze an intrinsic operation instruction.
    ///
    /// Handles: Intrinsic, TypeIntrinsic
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

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_intrinsic_ops called with non-intrinsic instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }

    /// Analyze a type intrinsic (@size_of, @align_of).
    fn analyze_type_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        type_arg: Spur,
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        let intrinsic_name = self.interner.resolve(&name);
        let ty = self.resolve_type(type_arg, span)?;

        // Calculate the value based on which intrinsic
        let value: u64 = match intrinsic_name {
            "size_of" => {
                // Calculate size in bytes (slot count * 8)
                let slot_count = self.abi_slot_count(ty);
                (slot_count * 8) as u64
            }
            "align_of" => {
                // Zero-sized types have 1-byte alignment, others have 8-byte
                let slot_count = self.abi_slot_count(ty);
                if slot_count == 0 { 1u64 } else { 8u64 }
            }
            _ => {
                return Err(CompileError::new(
                    ErrorKind::UnknownIntrinsic(intrinsic_name.to_string()),
                    span,
                ));
            }
        };

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Const(value),
            ty: Type::I32,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::I32))
    }

    // ========================================================================
    // Declaration no-ops: DropFnDecl, FnDecl
    // ========================================================================

    /// Analyze a declaration that produces Unit in expression context.
    ///
    /// Handles: DropFnDecl
    pub(crate) fn analyze_decl_noop(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        _ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::DropFnDecl { .. } => {
                // These are processed during collection phase, just return Unit
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty: Type::UNIT,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::UNIT))
            }

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

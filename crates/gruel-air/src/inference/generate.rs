//! Constraint generation for Hindley-Milner type inference.
//!
//! This module provides the constraint generation phase (Phase 1 of HM inference):
//! - [`ConstraintContext`] - Scoped variable tracking during generation
//! - [`ExprInfo`] - Result of constraint generation for an expression
//! - [`ConstraintGenerator`] - Walks RIR and generates type constraints
//! - Function/method signature types for type checking

use super::constraint::Constraint;
use super::types::{InferType, TypeVarAllocator, TypeVarId};
use crate::Type;
use crate::intern_pool::TypeInternPool;
use crate::scope::ScopedContext;
use crate::sema::InferenceContext;
use crate::types::{
    EnumId, PtrMutability, StructId, TypeKind, parse_array_type_syntax, parse_pointer_type_syntax,
    parse_type_call_syntax,
};
use gruel_builtins::BuiltinTypeConstructorKind;
use gruel_intrinsics::{IntrinsicId, lookup_by_name};
use gruel_rir::{InstData, InstRef, Rir};
use gruel_util::Span;
use gruel_util::{BinOp, UnaryOp};
use lasso::{Spur, ThreadedRodeo};
use rustc_hash::FxHashMap as HashMap;

/// Information about a local variable during constraint generation.
#[derive(Debug, Clone)]
pub struct LocalVarInfo {
    /// The inferred type of this variable.
    pub ty: InferType,
    /// Whether the variable is mutable.
    pub is_mut: bool,
    /// Span of the variable declaration.
    pub span: Span,
}

/// Information about a function parameter during constraint generation.
#[derive(Debug, Clone)]
pub struct ParamVarInfo {
    /// The type of this parameter, as InferType for uniform handling.
    pub ty: InferType,
}

/// Information about a function during constraint generation.
///
/// Uses `InferType` rather than `Type` so that array types are represented
/// structurally (as `InferType::Array { element, length }`) rather than by
/// opaque IDs. This allows uniform handling during inference.
#[derive(Debug, Clone)]
pub struct FunctionSig {
    /// Parameter types (in order), as InferTypes for uniform handling.
    pub param_types: Vec<InferType>,
    /// Return type, as InferType for uniform handling.
    pub return_type: InferType,
    /// Whether this is a generic function (has comptime type parameters).
    /// Generic functions skip type checking during constraint generation -
    /// they'll be checked during specialization.
    pub is_generic: bool,
    /// Parameter modes (Normal, Inout, Borrow, Comptime).
    pub param_modes: Vec<gruel_rir::RirParamMode>,
    /// Which parameters are comptime (declared with `comptime` keyword).
    /// This is separate from param_modes because `comptime T: type` sets
    /// is_comptime=true but mode=Normal.
    pub param_comptime: Vec<bool>,
    /// Parameter names, needed for type substitution in generic returns.
    pub param_names: Vec<lasso::Spur>,
    /// The return type as a symbol (used for substitution lookup).
    pub return_type_sym: lasso::Spur,
}

/// Information about a method during constraint generation.
///
/// Used for method calls (receiver.method()) and associated function calls (Type::function()).
#[derive(Debug, Clone)]
pub struct MethodSig {
    /// The struct type this method belongs to (as concrete Type::Struct)
    pub struct_type: Type,
    /// Whether this is a method (has self) or associated function (no self)
    pub has_self: bool,
    /// Parameter types (excluding self), as InferTypes for uniform handling.
    pub param_types: Vec<InferType>,
    /// Return type, as InferType for uniform handling.
    pub return_type: InferType,
}

/// Context for constraint generation within a single function.
pub struct ConstraintContext<'a> {
    /// Local variables in scope.
    pub locals: HashMap<Spur, LocalVarInfo>,
    /// Function parameters.
    pub params: &'a HashMap<Spur, ParamVarInfo>,
    /// Return type of the current function.
    pub return_type: Type,
    /// How many loops we're nested inside (for break/continue validation).
    pub loop_depth: u32,
    /// Scope stack for efficient scope management.
    scope_stack: Vec<Vec<(Spur, Option<LocalVarInfo>)>>,
}

impl<'a> ConstraintContext<'a> {
    /// Create a new context for a function.
    pub fn new(params: &'a HashMap<Spur, ParamVarInfo>, return_type: Type) -> Self {
        Self {
            locals: HashMap::default(),
            params,
            return_type,
            loop_depth: 0,
            scope_stack: Vec::new(),
        }
    }
}

impl ScopedContext for ConstraintContext<'_> {
    type VarInfo = LocalVarInfo;

    fn locals_mut(&mut self) -> &mut HashMap<Spur, Self::VarInfo> {
        &mut self.locals
    }

    fn scope_stack_mut(&mut self) -> &mut Vec<Vec<(Spur, Option<Self::VarInfo>)>> {
        &mut self.scope_stack
    }
}

/// Result of constraint generation for an expression.
#[derive(Debug, Clone)]
pub struct ExprInfo {
    /// The inferred type of this expression.
    pub ty: InferType,
    /// The span of this expression (for error reporting).
    pub span: Span,
}

impl ExprInfo {
    /// Create a new expression info.
    pub fn new(ty: InferType, span: Span) -> Self {
        Self { ty, span }
    }
}

/// Constraint generator that walks RIR and generates type constraints.
/// Return type of [`ConstraintGenerator::into_parts`].
pub type ConstraintGeneratorParts = (
    Vec<Constraint>,
    Vec<TypeVarId>,
    Vec<TypeVarId>,
    HashMap<InstRef, InferType>,
    u32,
);

///
/// This is Phase 1 of HM inference: constraint generation. The constraints
/// are later solved by the `Unifier` to determine concrete types.
pub struct ConstraintGenerator<'a> {
    /// The RIR being analyzed.
    rir: &'a Rir,
    /// String interner for resolving symbols.
    interner: &'a ThreadedRodeo,
    /// Type variable allocator.
    type_vars: TypeVarAllocator,
    /// Collected constraints.
    constraints: Vec<Constraint>,
    /// Mapping from RIR instruction to its inferred type.
    expr_types: HashMap<InstRef, InferType>,
    /// Function signatures (for call type checking).
    functions: &'a HashMap<Spur, FunctionSig>,
    /// Struct types (name -> Type::new_struct(id)).
    structs: &'a HashMap<Spur, Type>,
    /// Enum types (name -> Type::new_enum(id)).
    enums: &'a HashMap<Spur, Type>,
    /// Method signatures: (struct_id, method_name) -> MethodSig
    methods: &'a HashMap<(StructId, Spur), MethodSig>,
    /// Enum method signatures: (enum_id, method_name) -> MethodSig
    enum_methods: &'a HashMap<(EnumId, Spur), MethodSig>,
    /// Type variables allocated for integer literals.
    /// These start as unbound and need to be defaulted to i32 if unconstrained.
    int_literal_vars: Vec<TypeVarId>,
    /// Type variables allocated for float literals.
    /// These start as unbound and need to be defaulted to f64 if unconstrained.
    float_literal_vars: Vec<TypeVarId>,
    /// Type substitutions for Self and type parameters (used in method bodies).
    /// Maps type names (like "Self") to their concrete types.
    type_subst: Option<&'a HashMap<Spur, Type>>,
    /// Type intern pool for creating pointer and array types during constraint generation.
    type_pool: &'a TypeInternPool,
}

impl<'a> ConstraintGenerator<'a> {
    /// Create a new constraint generator.
    pub fn new(
        rir: &'a Rir,
        interner: &'a ThreadedRodeo,
        infer_ctx: &'a InferenceContext,
        type_pool: &'a TypeInternPool,
    ) -> Self {
        Self {
            rir,
            interner,
            type_vars: TypeVarAllocator::new(),
            constraints: Vec::new(),
            expr_types: HashMap::default(),
            functions: &infer_ctx.func_sigs,
            structs: &infer_ctx.struct_types,
            enums: &infer_ctx.enum_types,
            methods: &infer_ctx.method_sigs,
            enum_methods: &infer_ctx.enum_method_sigs,
            int_literal_vars: Vec::new(),
            float_literal_vars: Vec::new(),
            type_subst: None,
            type_pool,
        }
    }

    /// Set type substitutions for `Self` and type parameters (builder pattern).
    ///
    /// The `type_subst` map provides type substitutions for names like "Self"
    /// that should be resolved to concrete types during constraint generation.
    /// This is used for method bodies where `Self { ... }` struct literals
    /// need to know the concrete struct type.
    pub fn with_type_subst(mut self, type_subst: Option<&'a HashMap<Spur, Type>>) -> Self {
        self.type_subst = type_subst;
        self
    }

    /// Get the type variables allocated for integer literals.
    pub fn int_literal_vars(&self) -> &[TypeVarId] {
        &self.int_literal_vars
    }

    /// Allocate a fresh type variable.
    pub fn fresh_var(&mut self) -> TypeVarId {
        self.type_vars.fresh()
    }

    /// Add a constraint.
    pub fn add_constraint(&mut self, constraint: Constraint) {
        self.constraints.push(constraint);
    }

    /// Record the type of an expression.
    pub fn record_type(&mut self, inst_ref: InstRef, ty: InferType) {
        self.expr_types.insert(inst_ref, ty);
    }

    /// Get the recorded type of an expression.
    pub fn get_type(&self, inst_ref: InstRef) -> Option<&InferType> {
        self.expr_types.get(&inst_ref)
    }

    /// Get all collected constraints.
    pub fn constraints(&self) -> &[Constraint] {
        &self.constraints
    }

    /// Take ownership of the collected constraints.
    pub fn take_constraints(self) -> Vec<Constraint> {
        self.constraints
    }

    /// Get the expression type mapping.
    pub fn expr_types(&self) -> &HashMap<InstRef, InferType> {
        &self.expr_types
    }

    /// Consume the constraint generator and return (constraints, int_literal_vars, float_literal_vars, expr_types, type_var_count).
    ///
    /// This is useful when you need ownership of the expression types map.
    /// The `type_var_count` can be used to pre-size the unifier's substitution for better performance.
    pub fn into_parts(self) -> ConstraintGeneratorParts {
        (
            self.constraints,
            self.int_literal_vars,
            self.float_literal_vars,
            self.expr_types,
            self.type_vars.count(),
        )
    }

    /// Generate constraints for an expression.
    ///
    /// Returns the inferred type of the expression. Records the type in
    /// `expr_types` and adds constraints to `constraints`.
    pub fn generate(&mut self, inst_ref: InstRef, ctx: &mut ConstraintContext) -> ExprInfo {
        let inst = self.rir.get(inst_ref);
        let span = inst.span;

        let ty = match &inst.data {
            InstData::IntConst(_) => {
                // Integer literals get a fresh type variable that we immediately
                // bind to IntLiteral. This allows unification to track when the
                // literal is constrained to a specific integer type.
                //
                // Example: `let x: i64 = 42` generates:
                //   - type_var(?0) for the literal 42
                //   - substitution: ?0 -> IntLiteral
                //   - constraint: Equal(Var(?0), Concrete(i64))
                //
                // During unification, Equal(IntLiteral, Concrete(i64)) succeeds
                // and rebinds ?0 -> Concrete(i64) via rebind_int_literal_to_concrete.
                let var = self.fresh_var();
                self.int_literal_vars.push(var);
                InferType::Var(var)
            }

            InstData::FloatConst(_) => {
                // Float literals work like int literals but default to f64.
                let var = self.fresh_var();
                self.float_literal_vars.push(var);
                InferType::Var(var)
            }

            InstData::BoolConst(_) => InferType::Concrete(Type::BOOL),

            // String constants use the builtin String struct type.
            InstData::StringConst(_) => {
                // Look up the String type from the structs map
                if let Some(string_spur) = self.interner.get("String") {
                    if let Some(&string_ty) = self.structs.get(&string_spur) {
                        InferType::Concrete(string_ty)
                    } else {
                        // Fallback if String struct not found (shouldn't happen after builtin injection)
                        InferType::Concrete(Type::ERROR)
                    }
                } else {
                    InferType::Concrete(Type::ERROR)
                }
            }

            InstData::UnitConst => InferType::Concrete(Type::UNIT),

            InstData::Bin { op, lhs, rhs } => match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                    self.generate_binary_arith(*lhs, *rhs, ctx)
                }
                BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                    self.generate_binary_bitwise(*lhs, *rhs, ctx)
                }
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                    let lhs_info = self.generate(*lhs, ctx);
                    let rhs_info = self.generate(*rhs, ctx);
                    self.add_constraint(Constraint::equal(lhs_info.ty, rhs_info.ty, span));
                    InferType::Concrete(Type::BOOL)
                }
                BinOp::And | BinOp::Or => {
                    let lhs_info = self.generate(*lhs, ctx);
                    let rhs_info = self.generate(*rhs, ctx);
                    self.add_constraint(Constraint::equal(
                        lhs_info.ty,
                        InferType::Concrete(Type::BOOL),
                        lhs_info.span,
                    ));
                    self.add_constraint(Constraint::equal(
                        rhs_info.ty,
                        InferType::Concrete(Type::BOOL),
                        rhs_info.span,
                    ));
                    InferType::Concrete(Type::BOOL)
                }
            },

            InstData::Unary { op, operand } => match op {
                UnaryOp::Neg => {
                    let operand_info = self.generate(*operand, ctx);
                    let result_ty = operand_info.ty.clone();
                    self.add_constraint(Constraint::is_signed(result_ty.clone(), span));
                    result_ty
                }
                UnaryOp::Not => {
                    let operand_info = self.generate(*operand, ctx);
                    self.add_constraint(Constraint::equal(
                        operand_info.ty,
                        InferType::Concrete(Type::BOOL),
                        operand_info.span,
                    ));
                    InferType::Concrete(Type::BOOL)
                }
                UnaryOp::BitNot => {
                    let operand_info = self.generate(*operand, ctx);
                    let result_ty = operand_info.ty.clone();
                    self.add_constraint(Constraint::is_integer(result_ty.clone(), span));
                    result_ty
                }
            },

            // ADR-0062: `&x` / `&mut x` produces `Ref(T)` / `MutRef(T)`.
            // The result type depends on the operand's resolved type, which
            // inference may not have nailed down yet. Defer the construction
            // of the actual `Ref`/`MutRef` type to sema (`analyze_inst`),
            // and just propagate a fresh type variable that sema will set.
            InstData::MakeRef { operand, .. } => {
                let _ = self.generate(*operand, ctx);
                InferType::Var(self.fresh_var())
            }

            // ADR-0064: `&arr[range]` / `&mut arr[range]` produces a slice.
            // Defer the actual `Slice(T)` / `MutSlice(T)` type to sema; record
            // the sub-expressions so they receive types.
            InstData::MakeSlice { base, lo, hi, .. } => {
                let _ = self.generate(*base, ctx);
                if let Some(lo) = lo {
                    self.generate(*lo, ctx);
                }
                if let Some(hi) = hi {
                    self.generate(*hi, ctx);
                }
                InferType::Var(self.fresh_var())
            }

            // ADR-0064: a range subscript without `&` / `&mut`. Sema rejects.
            InstData::BareRangeSubscript => InferType::Concrete(Type::ERROR),

            // Variable reference
            InstData::VarRef { name } => {
                if let Some(local) = ctx.locals.get(name) {
                    local.ty.clone()
                } else if let Some(param) = ctx.params.get(name) {
                    param.ty.clone()
                } else {
                    // Unknown variable - will be caught during semantic analysis
                    InferType::Concrete(Type::ERROR)
                }
            }

            // Parameter reference
            InstData::ParamRef { name, .. } => {
                if let Some(param) = ctx.params.get(name) {
                    param.ty.clone()
                } else {
                    InferType::Concrete(Type::ERROR)
                }
            }

            // Local variable allocation
            InstData::Alloc {
                directives_start: _,
                directives_len: _,
                name,
                is_mut,
                ty: type_annotation,
                init,
            } => {
                let init_info = self.generate(*init, ctx);

                let var_ty = if let Some(ty_sym) = type_annotation {
                    // Explicit type annotation - use it and constrain init to match
                    let ty_name = self.interner.resolve(ty_sym);
                    if let Some(annotated_ty) = self.resolve_type_name(ty_name) {
                        self.add_constraint(Constraint::equal(
                            init_info.ty,
                            annotated_ty.clone(),
                            span,
                        ));
                        annotated_ty
                    } else {
                        // Unknown type name (e.g., struct/enum) - use init type for now.
                        // Semantic analysis will catch undefined types and verify struct/enum
                        // field types match the definition.
                        init_info.ty
                    }
                } else {
                    // No annotation - use the init expression's type
                    init_info.ty
                };

                // Record the variable in scope (if it has a name)
                if let Some(var_name) = name {
                    ctx.insert_local(
                        *var_name,
                        LocalVarInfo {
                            ty: var_ty.clone(),
                            is_mut: *is_mut,
                            span,
                        },
                    );
                }

                // Alloc produces unit type
                InferType::Concrete(Type::UNIT)
            }

            // Struct destructuring — register field bindings for type inference
            InstData::StructDestructure {
                type_name,
                fields_start,
                fields_len,
                init,
            } => {
                self.generate(*init, ctx);

                // Look up the struct type to get field types
                if let Some(&struct_ty) = self.structs.get(type_name)
                    && let Some(struct_id) = struct_ty.as_struct()
                {
                    let struct_def = self.type_pool.struct_def(struct_id);
                    let rir_fields = self.rir.get_destructure_fields(*fields_start, *fields_len);
                    for field in &rir_fields {
                        if field.is_wildcard {
                            continue;
                        }
                        let field_name = self.interner.resolve(&field.field_name);
                        if let Some((_, struct_field)) = struct_def.find_field(field_name) {
                            let binding_name = field.binding_name.unwrap_or(field.field_name);
                            ctx.insert_local(
                                binding_name,
                                LocalVarInfo {
                                    ty: InferType::Concrete(struct_field.ty),
                                    is_mut: field.is_mut,
                                    span,
                                },
                            );
                        }
                    }
                }

                InferType::Concrete(Type::UNIT)
            }

            // Assignment
            InstData::Assign { name, value } => {
                let value_info = self.generate(*value, ctx);
                if let Some(local) = ctx.locals.get(name) {
                    // Constrain value to match variable type
                    self.add_constraint(Constraint::equal(value_info.ty, local.ty.clone(), span));
                }
                // Assignment produces unit
                InferType::Concrete(Type::UNIT)
            }

            // Return statement
            InstData::Ret(value) => {
                if let Some(val_ref) = value {
                    let value_info = self.generate(*val_ref, ctx);
                    // Constrain return value to match function return type
                    self.add_constraint(Constraint::equal(
                        value_info.ty,
                        InferType::Concrete(ctx.return_type),
                        span,
                    ));
                } else {
                    // Return without value - function must return unit
                    self.add_constraint(Constraint::equal(
                        InferType::Concrete(Type::UNIT),
                        InferType::Concrete(ctx.return_type),
                        span,
                    ));
                }
                // Return diverges
                InferType::Concrete(Type::NEVER)
            }

            // Function call
            InstData::Call {
                name,
                args_start,
                args_len,
            } => {
                let args = self.rir.get_call_args(*args_start, *args_len);
                if let Some(func) = self.functions.get(name) {
                    // For generic functions, skip constraint generation for arguments.
                    // The types will be checked during specialization when we know
                    // the concrete type substitutions.
                    if func.is_generic {
                        // Process all arguments and build type substitution map
                        let mut type_subst: rustc_hash::FxHashMap<lasso::Spur, Type> =
                            rustc_hash::FxHashMap::default();

                        for (i, arg) in args.iter().enumerate() {
                            let arg_info = self.generate(arg.value, ctx);

                            // If this is a comptime parameter, extract the type for substitution
                            if i < func.param_comptime.len() && func.param_comptime[i] {
                                // The argument should be a TypeConst - extract the concrete type
                                if let InferType::Concrete(Type::COMPTIME_TYPE) = &arg_info.ty {
                                    // This is a type value - get the actual type from the RIR
                                    let arg_inst = self.rir.get(arg.value);
                                    if let gruel_rir::InstData::TypeConst { type_name } =
                                        &arg_inst.data
                                    {
                                        // Resolve type_name to a concrete Type
                                        let type_name_str = self.interner.resolve(type_name);
                                        let concrete_ty = match type_name_str {
                                            "i8" => Type::I8,
                                            "i16" => Type::I16,
                                            "i32" => Type::I32,
                                            "i64" => Type::I64,
                                            "u8" => Type::U8,
                                            "u16" => Type::U16,
                                            "u32" => Type::U32,
                                            "u64" => Type::U64,
                                            "bool" => Type::BOOL,
                                            "()" => Type::UNIT,
                                            _ => Type::ERROR, // Unknown type
                                        };
                                        if i < func.param_names.len() {
                                            type_subst.insert(func.param_names[i], concrete_ty);
                                        }
                                    }
                                }
                            }
                        }

                        // Compute the actual return type by substituting type parameters

                        if func.return_type == InferType::Concrete(Type::COMPTIME_TYPE) {
                            // Return type is a type parameter - look it up in substitutions
                            if let Some(&concrete_ty) = type_subst.get(&func.return_type_sym) {
                                InferType::Concrete(concrete_ty)
                            } else {
                                func.return_type.clone()
                            }
                        } else {
                            func.return_type.clone()
                        }
                    } else if args.len() != func.param_types.len() {
                        // Check argument count matches parameter count.
                        // Semantic analysis will emit a proper error; we just need to avoid
                        // panicking and process what we can.
                        // Still process all arguments to catch type errors within them
                        for arg in args.iter() {
                            self.generate(arg.value, ctx);
                        }
                        // Return the declared return type (error will be caught in sema)
                        func.return_type.clone()
                    } else {
                        // Generate constraints for each argument.
                        // ADR-0056: skip the equality constraint when the
                        // parameter type is an interface — sema applies
                        // structural conformance (not equality) and inserts
                        // a `MakeInterfaceRef` coercion at the call site.
                        for (arg, param_ty) in args.iter().zip(func.param_types.iter()) {
                            let arg_info = self.generate(arg.value, ctx);
                            let is_iface = matches!(
                                param_ty,
                                InferType::Concrete(t) if matches!(
                                    t.kind(),
                                    crate::types::TypeKind::Interface(_)
                                )
                            );
                            if !is_iface {
                                self.add_constraint(Constraint::equal(
                                    arg_info.ty,
                                    param_ty.clone(),
                                    arg_info.span,
                                ));
                            }
                        }
                        func.return_type.clone()
                    }
                } else {
                    // Unknown function - still process arguments for constraint generation
                    for arg in args.iter() {
                        self.generate(arg.value, ctx);
                    }
                    InferType::Concrete(Type::ERROR)
                }
            }

            // Intrinsic call
            InstData::Intrinsic {
                name,
                args_start,
                args_len,
            } => {
                let intrinsic_name = self.interner.resolve(name);
                let id = lookup_by_name(intrinsic_name).map(|d| d.id);
                // Collect arg InstRefs so we can iterate without holding a
                // borrow on self.rir across the dispatch match.
                let arg_refs: Vec<InstRef> =
                    self.rir.get_inst_refs(*args_start, *args_len).to_vec();

                // Visit args in a side-effectful pass so constraints on them
                // are emitted regardless of which intrinsic we hit below.
                let visit_args = |this: &mut Self, ctx: &mut ConstraintContext| {
                    for &arg_ref in arg_refs.iter() {
                        this.generate(arg_ref, ctx);
                    }
                };

                match id {
                    Some(IntrinsicId::Cast) => {
                        if let Some(&first) = arg_refs.first() {
                            let _ = self.generate(first, ctx);
                        }
                        InferType::Var(self.fresh_var())
                    }
                    Some(IntrinsicId::ReadLine) => {
                        if let Some(string_spur) = self.interner.get("String") {
                            if let Some(&string_ty) = self.structs.get(&string_spur) {
                                InferType::Concrete(string_ty)
                            } else {
                                InferType::Concrete(Type::ERROR)
                            }
                        } else {
                            InferType::Concrete(Type::ERROR)
                        }
                    }
                    Some(IntrinsicId::ParseI32) => {
                        visit_args(self, ctx);
                        InferType::Concrete(Type::I32)
                    }
                    Some(IntrinsicId::ParseI64) => {
                        visit_args(self, ctx);
                        InferType::Concrete(Type::I64)
                    }
                    Some(IntrinsicId::ParseU32) => {
                        visit_args(self, ctx);
                        InferType::Concrete(Type::U32)
                    }
                    Some(IntrinsicId::ParseU64) => {
                        visit_args(self, ctx);
                        InferType::Concrete(Type::U64)
                    }
                    Some(IntrinsicId::RandomU32) => InferType::Concrete(Type::U32),
                    Some(IntrinsicId::RandomU64) => InferType::Concrete(Type::U64),
                    Some(IntrinsicId::Syscall) => {
                        visit_args(self, ctx);
                        InferType::Concrete(Type::I64)
                    }
                    Some(IntrinsicId::PtrToInt) => {
                        visit_args(self, ctx);
                        InferType::Concrete(Type::U64)
                    }
                    Some(IntrinsicId::PtrWrite) => {
                        visit_args(self, ctx);
                        InferType::Concrete(Type::UNIT)
                    }
                    Some(IntrinsicId::IsNull) => {
                        visit_args(self, ctx);
                        InferType::Concrete(Type::BOOL)
                    }
                    Some(IntrinsicId::PtrRead) => {
                        // Return type depends on pointee type of the argument —
                        // resolved in sema once the concrete pointer type is known.
                        visit_args(self, ctx);
                        InferType::Var(self.fresh_var())
                    }
                    Some(IntrinsicId::PtrOffset) => {
                        // Return type matches the input pointer type.
                        visit_args(self, ctx);
                        InferType::Var(self.fresh_var())
                    }
                    Some(IntrinsicId::Raw) | Some(IntrinsicId::RawMut) => {
                        // Returns ptr const T / ptr mut T — resolved in sema.
                        visit_args(self, ctx);
                        InferType::Var(self.fresh_var())
                    }
                    Some(IntrinsicId::IntToPtr) | Some(IntrinsicId::NullPtr) => {
                        // Pointer type inferred from context.
                        visit_args(self, ctx);
                        InferType::Var(self.fresh_var())
                    }
                    Some(IntrinsicId::PtrCopy) => {
                        visit_args(self, ctx);
                        InferType::Concrete(Type::UNIT)
                    }
                    Some(IntrinsicId::TargetArch) => {
                        if let Some(arch_spur) = self.interner.get("Arch") {
                            if let Some(&arch_ty) = self.enums.get(&arch_spur) {
                                InferType::Concrete(arch_ty)
                            } else {
                                InferType::Concrete(Type::ERROR)
                            }
                        } else {
                            InferType::Concrete(Type::ERROR)
                        }
                    }
                    Some(IntrinsicId::TargetOs) => {
                        if let Some(os_spur) = self.interner.get("Os") {
                            if let Some(&os_ty) = self.enums.get(&os_spur) {
                                InferType::Concrete(os_ty)
                            } else {
                                InferType::Concrete(Type::ERROR)
                            }
                        } else {
                            InferType::Concrete(Type::ERROR)
                        }
                    }
                    Some(IntrinsicId::Range) => {
                        // @range: 1-3 integer args; returns the same integer type
                        // (used as an iterable in for-in loops).
                        if let Some((&first_ref, rest)) = arg_refs.split_first() {
                            let first = self.generate(first_ref, ctx);
                            for &arg_ref in rest {
                                let arg_info = self.generate(arg_ref, ctx);
                                self.add_constraint(Constraint::equal(
                                    first.ty.clone(),
                                    arg_info.ty,
                                    span,
                                ));
                            }
                            first.ty
                        } else {
                            InferType::Concrete(Type::ERROR)
                        }
                    }
                    Some(IntrinsicId::Field) => {
                        // Return type depends on which field is accessed — fresh var.
                        visit_args(self, ctx);
                        InferType::Var(self.fresh_var())
                    }
                    // ADR-0064: slice intrinsics. Sema produces the actual
                    // type once it has the receiver/argument types resolved;
                    // here we just emit a fresh variable.
                    Some(IntrinsicId::SliceLen) => {
                        visit_args(self, ctx);
                        InferType::Concrete(Type::USIZE)
                    }
                    Some(IntrinsicId::SliceIsEmpty) => {
                        visit_args(self, ctx);
                        InferType::Concrete(Type::BOOL)
                    }
                    Some(IntrinsicId::SliceIndexRead) => {
                        visit_args(self, ctx);
                        InferType::Var(self.fresh_var())
                    }
                    Some(IntrinsicId::SliceIndexWrite) => {
                        visit_args(self, ctx);
                        InferType::Concrete(Type::UNIT)
                    }
                    Some(IntrinsicId::SlicePtr)
                    | Some(IntrinsicId::SlicePtrMut)
                    | Some(IntrinsicId::PartsToSlice)
                    | Some(IntrinsicId::PartsToMutSlice) => {
                        visit_args(self, ctx);
                        InferType::Var(self.fresh_var())
                    }
                    Some(IntrinsicId::EmbedFile) => {
                        // `@embed_file("path")` always produces a `Slice(u8)`,
                        // independent of context. Visiting args is a no-op
                        // here (string literal) but kept for consistency.
                        visit_args(self, ctx);
                        let slice_id = self.type_pool.intern_slice_from_type(Type::U8);
                        InferType::Concrete(Type::new_slice(slice_id))
                    }
                    Some(IntrinsicId::Panic) | Some(IntrinsicId::CompileError) => {
                        // Diverging intrinsics return Never so they unify with any
                        // expected type (e.g. `if c { 42 } else { @panic("..") }`).
                        visit_args(self, ctx);
                        InferType::Concrete(Type::NEVER)
                    }
                    // Other intrinsics (@dbg, @assert, @test_preview_gate, @import)
                    // and any unknown name return Unit. Sema handles the unknown case
                    // with a proper diagnostic; we just pick a coherent type here.
                    _ => {
                        visit_args(self, ctx);
                        InferType::Concrete(Type::UNIT)
                    }
                }
            }

            // Type intrinsic (@size_of, @align_of, @type_name, @type_info, @ownership)
            InstData::TypeIntrinsic { name, type_arg: _ } => {
                let intrinsic_name = self.interner.resolve(name);
                match lookup_by_name(intrinsic_name).map(|d| d.id) {
                    Some(IntrinsicId::TypeName) => InferType::Concrete(Type::COMPTIME_STR),
                    Some(IntrinsicId::TypeInfo) => {
                        // @type_info returns a comptime struct — use a fresh var
                        // since the actual type is determined by the comptime evaluator.
                        InferType::Var(self.fresh_var())
                    }
                    Some(IntrinsicId::SizeOf) | Some(IntrinsicId::AlignOf) => {
                        // @size_of / @align_of return `usize` (ADR-0054).
                        InferType::Concrete(Type::USIZE)
                    }
                    Some(IntrinsicId::Ownership) => {
                        // @ownership returns the built-in `Ownership` enum.
                        if let Some(ownership_spur) = self.interner.get("Ownership") {
                            if let Some(&ownership_ty) = self.enums.get(&ownership_spur) {
                                InferType::Concrete(ownership_ty)
                            } else {
                                InferType::Concrete(Type::ERROR)
                            }
                        } else {
                            InferType::Concrete(Type::ERROR)
                        }
                    }
                    // Fallback for unknown names.
                    _ => InferType::Concrete(Type::I32),
                }
            }

            // Type+interface intrinsic (@conforms)
            InstData::TypeInterfaceIntrinsic { name, .. } => {
                let intrinsic_name = self.interner.resolve(name);
                match lookup_by_name(intrinsic_name).map(|d| d.id) {
                    Some(IntrinsicId::Conforms) => InferType::Concrete(Type::BOOL),
                    _ => InferType::Concrete(Type::ERROR),
                }
            }

            // Block
            InstData::Block { extra_start, len } => {
                ctx.push_scope();
                let mut last_ty = InferType::Concrete(Type::UNIT);
                let block_insts = self.rir.get_extra(*extra_start, *len);
                for &inst_raw in block_insts {
                    let block_inst_ref = InstRef::from_raw(inst_raw);
                    let info = self.generate(block_inst_ref, ctx);
                    last_ty = info.ty;
                }
                ctx.pop_scope();
                last_ty
            }

            // Branch (if/else)
            InstData::Branch {
                cond,
                then_block,
                else_block,
            } => {
                let cond_info = self.generate(*cond, ctx);
                self.add_constraint(Constraint::equal(
                    cond_info.ty,
                    InferType::Concrete(Type::BOOL),
                    cond_info.span,
                ));

                let then_info = self.generate(*then_block, ctx);

                if let Some(else_ref) = else_block {
                    let else_info = self.generate(*else_ref, ctx);

                    // Handle Never type coercion:
                    // - If one branch is Never, the if-else takes the other branch's type
                    // - If both are Never, the result is Never
                    // - Otherwise, both must unify to the same type
                    let then_is_never = matches!(&then_info.ty, InferType::Concrete(Type::NEVER));
                    let else_is_never = matches!(&else_info.ty, InferType::Concrete(Type::NEVER));

                    match (then_is_never, else_is_never) {
                        (true, true) => {
                            // Both diverge - result is Never
                            InferType::Concrete(Type::NEVER)
                        }
                        (true, false) => {
                            // Then diverges - result is else type
                            else_info.ty
                        }
                        (false, true) => {
                            // Else diverges - result is then type
                            then_info.ty
                        }
                        (false, false) => {
                            // Neither diverges - both must have the same type
                            let result_var = self.fresh_var();
                            let result_ty = InferType::Var(result_var);
                            self.add_constraint(Constraint::equal(
                                then_info.ty,
                                result_ty.clone(),
                                then_info.span,
                            ));
                            self.add_constraint(Constraint::equal(
                                else_info.ty,
                                result_ty.clone(),
                                else_info.span,
                            ));
                            result_ty
                        }
                    }
                } else {
                    // No else branch - the if expression has unit type
                    // (or the then branch type if it's unit-compatible)
                    InferType::Concrete(Type::UNIT)
                }
            }

            // While loop
            InstData::Loop { cond, body } => {
                let cond_info = self.generate(*cond, ctx);
                self.add_constraint(Constraint::equal(
                    cond_info.ty,
                    InferType::Concrete(Type::BOOL),
                    cond_info.span,
                ));

                ctx.loop_depth += 1;
                self.generate(*body, ctx);
                ctx.loop_depth -= 1;

                // Loops produce unit
                InferType::Concrete(Type::UNIT)
            }

            // For-in loop (desugared to while in sema, but inference still sees it)
            InstData::For {
                binding,
                is_mut,
                iterable,
                body,
            } => {
                // Generate constraints for the iterable to determine the element type
                let iterable_info = self.generate(*iterable, ctx);

                // Determine the binding type from the iterable:
                // - For @range: the iterable returns the integer type directly
                // - For arrays: extract the element type from InferType::Array
                // - For slices (ADR-0064): extract the element from
                //   `Slice(T)` / `MutSlice(T)`
                let binding_ty = match &iterable_info.ty {
                    InferType::Array { element, .. } => *element.clone(),
                    InferType::Concrete(t) => match t.kind() {
                        TypeKind::Slice(id) => InferType::Concrete(self.type_pool.slice_def(id)),
                        TypeKind::MutSlice(id) => {
                            InferType::Concrete(self.type_pool.mut_slice_def(id))
                        }
                        _ => iterable_info.ty.clone(),
                    },
                    other => other.clone(),
                };

                // Register the binding so the body can reference it
                ctx.insert_local(
                    *binding,
                    LocalVarInfo {
                        ty: binding_ty,
                        is_mut: *is_mut,
                        span,
                    },
                );

                ctx.loop_depth += 1;
                self.generate(*body, ctx);
                ctx.loop_depth -= 1;

                // For loops produce unit
                InferType::Concrete(Type::UNIT)
            }

            // Infinite loop
            InstData::InfiniteLoop { body } => {
                ctx.loop_depth += 1;
                self.generate(*body, ctx);
                ctx.loop_depth -= 1;

                // Infinite loop without break never returns
                InferType::Concrete(Type::NEVER)
            }

            // Break/Continue
            InstData::Break | InstData::Continue => InferType::Concrete(Type::NEVER),

            // Match expression
            InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            } => {
                let scrutinee_info = self.generate(*scrutinee, ctx);
                let arms = self.rir.get_match_arms(*arms_start, *arms_len);

                // Collect arm types, handling Never coercion
                let mut arm_types: Vec<ExprInfo> = Vec::new();
                for (pattern, body) in arms.iter() {
                    // Patterns constrain the scrutinee type
                    let pattern_ty = self.pattern_type(pattern);
                    self.add_constraint(Constraint::equal(
                        scrutinee_info.ty.clone(),
                        pattern_ty,
                        pattern.span(),
                    ));

                    // For DataVariant/StructVariant patterns, add bound variables to scope before
                    // generating body constraints, so VarRef lookups resolve correctly.
                    let bindings_to_remove = match pattern {
                        gruel_rir::RirPattern::DataVariant {
                            type_name,
                            variant,
                            bindings,
                            ..
                        } => {
                            let mut added_bindings = Vec::new();
                            if let Some(&enum_ty) = self.enums.get(type_name)
                                && let Some(enum_id) = enum_ty.as_enum()
                            {
                                let enum_def = self.type_pool.enum_def(enum_id);
                                let variant_name = self.interner.resolve(variant);
                                if let Some(variant_idx) = enum_def.find_variant(variant_name) {
                                    let field_types = &enum_def.variants[variant_idx].fields;
                                    for (i, binding) in bindings.iter().enumerate() {
                                        let field_ty = if i < field_types.len() {
                                            InferType::Concrete(field_types[i])
                                        } else {
                                            InferType::Concrete(Type::ERROR)
                                        };
                                        self.register_binding(
                                            binding,
                                            field_ty,
                                            pattern.span(),
                                            ctx,
                                            &mut added_bindings,
                                        );
                                    }
                                }
                            } else {
                                // Enum not found — likely a comptime type variable.
                                // Register bindings with fresh type variables so body
                                // constraint generation can still resolve variable references.
                                for binding in bindings.iter() {
                                    let var = self.fresh_var();
                                    let ty = InferType::Var(var);
                                    self.register_binding(
                                        binding,
                                        ty,
                                        pattern.span(),
                                        ctx,
                                        &mut added_bindings,
                                    );
                                }
                            }
                            added_bindings
                        }
                        gruel_rir::RirPattern::StructVariant {
                            type_name,
                            variant,
                            field_bindings,
                            ..
                        } => {
                            let mut added_bindings = Vec::new();
                            if let Some(&enum_ty) = self.enums.get(type_name)
                                && let Some(enum_id) = enum_ty.as_enum()
                            {
                                let enum_def = self.type_pool.enum_def(enum_id);
                                let variant_name = self.interner.resolve(variant);
                                if let Some(variant_idx) = enum_def.find_variant(variant_name) {
                                    let variant_def = &enum_def.variants[variant_idx];
                                    for fb in field_bindings {
                                        let field_name = self.interner.resolve(&fb.field_name);
                                        let field_ty =
                                            if let Some(idx) = variant_def.find_field(field_name) {
                                                InferType::Concrete(variant_def.fields[idx])
                                            } else {
                                                InferType::Concrete(Type::ERROR)
                                            };
                                        self.register_binding(
                                            &fb.binding,
                                            field_ty,
                                            pattern.span(),
                                            ctx,
                                            &mut added_bindings,
                                        );
                                    }
                                }
                            } else {
                                // Enum not found — likely a comptime type variable.
                                // Register bindings with fresh type variables.
                                for fb in field_bindings {
                                    let var = self.fresh_var();
                                    let ty = InferType::Var(var);
                                    self.register_binding(
                                        &fb.binding,
                                        ty,
                                        pattern.span(),
                                        ctx,
                                        &mut added_bindings,
                                    );
                                }
                            }
                            added_bindings
                        }
                        // ADR-0051 Phase 4c: register Ident-leaf bindings for
                        // Tuple / Struct / Ident arm roots. We walk the
                        // pattern tree recursively, pulling field types from
                        // the scrutinee's concrete struct definition when
                        // available; unknown types get fresh variables so
                        // body constraint generation still finds the binding.
                        gruel_rir::RirPattern::Ident { .. }
                        | gruel_rir::RirPattern::Tuple { .. }
                        | gruel_rir::RirPattern::Struct { .. } => {
                            let mut added_bindings = Vec::new();
                            self.collect_recursive_pattern_bindings(
                                pattern,
                                scrutinee_info.ty.clone(),
                                ctx,
                                &mut added_bindings,
                            );
                            added_bindings
                        }
                        _ => Vec::new(),
                    };

                    // Generate body and collect its type
                    let body_info = self.generate(*body, ctx);
                    arm_types.push(body_info);

                    // Remove DataVariant bindings from scope after body generation
                    for (name, old_val) in bindings_to_remove {
                        match old_val {
                            Some(prev) => {
                                ctx.locals.insert(name, prev);
                            }
                            None => {
                                ctx.locals.remove(&name);
                            }
                        }
                    }
                }

                // Handle Never type coercion:
                // Filter out Never arms and use the remaining non-Never types
                let non_never_arms: Vec<_> = arm_types
                    .iter()
                    .filter(|info| !matches!(&info.ty, InferType::Concrete(Type::NEVER)))
                    .collect();

                if non_never_arms.is_empty() {
                    // All arms diverge - result is Never
                    InferType::Concrete(Type::NEVER)
                } else {
                    // Create constraints for non-Never arms to have the same type
                    let result_var = self.fresh_var();
                    let result_ty = InferType::Var(result_var);
                    for arm_info in non_never_arms {
                        self.add_constraint(Constraint::equal(
                            arm_info.ty.clone(),
                            result_ty.clone(),
                            arm_info.span,
                        ));
                    }
                    result_ty
                }
            }

            // Struct initialization
            InstData::StructInit {
                type_name,
                fields_start,
                fields_len,
                ..
            } => {
                // Check type_subst first (for Self and type parameters in method bodies)
                let struct_ty = self
                    .type_subst
                    .and_then(|subst| subst.get(type_name).copied())
                    .or_else(|| self.structs.get(type_name).copied());

                if let Some(struct_ty) = struct_ty {
                    let fields = self.rir.get_field_inits(*fields_start, *fields_len);
                    // Generate constraints for each field
                    for (_, value_ref) in fields.iter() {
                        self.generate(*value_ref, ctx);
                    }
                    InferType::Concrete(struct_ty)
                } else {
                    InferType::Concrete(Type::ERROR)
                }
            }

            // Field access
            InstData::FieldGet { base, field: _ } => {
                // Generate constraints for the base expression (needed for nested field access)
                let _base_info = self.generate(*base, ctx);
                // We need to look up the field type from the struct definition.
                // For now, use a fresh type variable - full resolution happens during
                // semantic analysis which has access to struct definitions.
                let result_var = self.fresh_var();
                InferType::Var(result_var)
            }

            // Field assignment
            InstData::FieldSet {
                base,
                field: _,
                value,
            } => {
                self.generate(*base, ctx);
                self.generate(*value, ctx);
                InferType::Concrete(Type::UNIT)
            }

            // Enum variant (unit or path)
            InstData::EnumVariant { type_name, .. } => {
                if let Some(&enum_ty) = self.enums.get(type_name) {
                    InferType::Concrete(enum_ty)
                } else {
                    // May be a comptime type variable — use fresh var
                    let var = self.fresh_var();
                    InferType::Var(var)
                }
            }

            // Enum struct variant construction
            InstData::EnumStructVariant {
                type_name,
                fields_start,
                fields_len,
                ..
            } => {
                // Generate constraints for field value expressions
                let fields = self.rir.get_field_inits(*fields_start, *fields_len);
                for (_, value_ref) in fields.iter() {
                    self.generate(*value_ref, ctx);
                }
                if let Some(&enum_ty) = self.enums.get(type_name) {
                    InferType::Concrete(enum_ty)
                } else {
                    // May be a comptime type variable — use fresh var
                    let var = self.fresh_var();
                    InferType::Var(var)
                }
            }

            // Array initialization
            InstData::ArrayInit {
                elems_start,
                elems_len,
            } => {
                let elements = self.rir.get_inst_refs(*elems_start, *elems_len);
                if elements.is_empty() {
                    // Empty array - need type annotation to know element type
                    // Use a fresh type variable for the element type
                    let elem_var = self.fresh_var();
                    InferType::Array {
                        element: Box::new(InferType::Var(elem_var)),
                        length: 0,
                    }
                } else {
                    // Get element type from first element, constrain rest to match
                    let first_info = self.generate(elements[0], ctx);
                    for elem_ref in elements.iter().skip(1) {
                        let elem_info = self.generate(*elem_ref, ctx);
                        self.add_constraint(Constraint::equal(
                            elem_info.ty,
                            first_info.ty.clone(),
                            elem_info.span,
                        ));
                    }
                    // Build the array type with the inferred element type
                    InferType::Array {
                        element: Box::new(first_info.ty),
                        length: elements.len() as u64,
                    }
                }
            }

            // Array index
            InstData::IndexGet { base, index } => {
                let base_info = self.generate(*base, ctx);
                let index_info = self.generate(*index, ctx);
                // Index must be exactly `usize` (ADR-0054).
                self.add_constraint(Constraint::equal(
                    InferType::Concrete(Type::USIZE),
                    index_info.ty.clone(),
                    index_info.span,
                ));

                // Extract element type from array type.
                // If base is InferType::Array, we can get the element type directly.
                // Otherwise, we need a fresh variable that will be resolved later.
                match &base_info.ty {
                    InferType::Array { element, .. } => (**element).clone(),
                    _ => {
                        // Base might be a type variable that will resolve to an array.
                        // Use a fresh variable for the element type.
                        let result_var = self.fresh_var();
                        InferType::Var(result_var)
                    }
                }
            }

            // Array index assignment
            InstData::IndexSet { base, index, value } => {
                let base_info = self.generate(*base, ctx);
                let index_info = self.generate(*index, ctx);
                // Index must be exactly `usize` (ADR-0054).
                self.add_constraint(Constraint::equal(
                    InferType::Concrete(Type::USIZE),
                    index_info.ty.clone(),
                    index_info.span,
                ));

                let value_info = self.generate(*value, ctx);

                // Constrain value type to match array element type
                if let InferType::Array { element, .. } = &base_info.ty {
                    self.add_constraint(Constraint::equal(
                        value_info.ty,
                        (**element).clone(),
                        value_info.span,
                    ));
                }

                InferType::Concrete(Type::UNIT)
            }

            // Type declarations don't produce values
            InstData::FnDecl { .. }
            | InstData::StructDecl { .. }
            | InstData::EnumDecl { .. }
            | InstData::InterfaceDecl { .. }
            | InstData::InterfaceMethodSig { .. }
            | InstData::DeriveDecl { .. }
            | InstData::DropFnDecl { .. }
            | InstData::ConstDecl { .. } => InferType::Concrete(Type::UNIT),

            // ADR-0057: anonymous interface type expressions yield a
            // comptime type value (parallel to AnonStructType /
            // AnonEnumType, which are typed COMPTIME_TYPE elsewhere via
            // their dedicated arms).
            InstData::AnonInterfaceType { .. } => InferType::Concrete(Type::COMPTIME_TYPE),

            // Method call: receiver.method(args)
            InstData::MethodCall {
                receiver,
                method,
                args_start,
                args_len,
            } => {
                // Generate type for receiver
                let receiver_info = self.generate(*receiver, ctx);
                let args = self.rir.get_call_args(*args_start, *args_len);

                // Get struct name from receiver type if it's a struct
                // If we can't determine the struct type, we still generate constraints
                // for the arguments and return a type variable (actual error is in sema)

                if let InferType::Concrete(ty) = &receiver_info.ty {
                    // ADR-0063: methods on `Ptr(T)` / `MutPtr(T)` resolve via
                    // the POINTER_METHODS registry. Fan out per method here
                    // so HM produces the right constraint shape; sema does
                    // the real type-checking.
                    if matches!(ty.kind(), TypeKind::PtrConst(_) | TypeKind::PtrMut(_)) {
                        let method_str = self.interner.resolve(method);
                        let pointee = match ty.kind() {
                            TypeKind::PtrConst(id) => self.type_pool.ptr_const_def(id),
                            TypeKind::PtrMut(id) => self.type_pool.ptr_mut_def(id),
                            _ => unreachable!(),
                        };
                        // Generate inference for args (no constraints — sema
                        // will type-check).
                        for arg in args.iter() {
                            self.generate(arg.value, ctx);
                        }
                        return ExprInfo {
                            ty: match method_str {
                                "read" => InferType::Concrete(pointee),
                                "write" => InferType::Concrete(Type::UNIT),
                                "offset" => InferType::Concrete(*ty),
                                "is_null" => InferType::Concrete(Type::BOOL),
                                "to_int" => InferType::Concrete(Type::U64),
                                "copy_from" => InferType::Concrete(Type::UNIT),
                                _ => InferType::Concrete(Type::ERROR),
                            },
                            span,
                        };
                    }

                    if let Some(struct_id) = ty.as_struct() {
                        // Use StructId directly for method lookup
                        let method_key = (struct_id, *method);
                        if let Some(method_sig) = self.methods.get(&method_key) {
                            // Generate constraints for arguments
                            for (arg, param_type) in args.iter().zip(method_sig.param_types.iter())
                            {
                                let arg_info = self.generate(arg.value, ctx);
                                self.add_constraint(Constraint::equal(
                                    arg_info.ty,
                                    param_type.clone(),
                                    arg_info.span,
                                ));
                            }
                            method_sig.return_type.clone()
                        } else {
                            // Method not found in pre-built context - may be an anonymous
                            // struct method registered during comptime evaluation.
                            // Use a fresh type variable so inference doesn't poison
                            // surrounding expressions with ERROR.
                            for arg in args.iter() {
                                self.generate(arg.value, ctx);
                            }
                            InferType::Var(self.fresh_var())
                        }
                    } else if let Some(enum_id) = ty.as_enum() {
                        // Enum receiver - check enum_methods
                        let method_key = (enum_id, *method);
                        if let Some(method_sig) = self.enum_methods.get(&method_key) {
                            for (arg, param_type) in args.iter().zip(method_sig.param_types.iter())
                            {
                                let arg_info = self.generate(arg.value, ctx);
                                self.add_constraint(Constraint::equal(
                                    arg_info.ty,
                                    param_type.clone(),
                                    arg_info.span,
                                ));
                            }
                            method_sig.return_type.clone()
                        } else {
                            // Enum method not found - use fresh var, sema reports error
                            for arg in args.iter() {
                                self.generate(arg.value, ctx);
                            }
                            InferType::Var(self.fresh_var())
                        }
                    } else {
                        // Non-struct/non-enum receiver - sema will report the error
                        for arg in args.iter() {
                            self.generate(arg.value, ctx);
                        }
                        InferType::Concrete(Type::ERROR)
                    }
                } else {
                    // Non-concrete receiver type - use fresh var, sema resolves it
                    for arg in args.iter() {
                        self.generate(arg.value, ctx);
                    }
                    InferType::Var(self.fresh_var())
                }
            }

            // Associated function call: Type::function(args)
            InstData::AssocFnCall {
                type_name,
                function,
                args_start,
                args_len,
            } => {
                let args = self.rir.get_call_args(*args_start, *args_len);
                // Get struct ID from type name for method lookup
                let struct_id = self.structs.get(type_name).and_then(|ty| ty.as_struct());

                if let Some(struct_id) = struct_id {
                    let method_key = (struct_id, *function);
                    if let Some(method_sig) = self.methods.get(&method_key) {
                        // Generate constraints for arguments
                        for (arg, param_type) in args.iter().zip(method_sig.param_types.iter()) {
                            let arg_info = self.generate(arg.value, ctx);
                            self.add_constraint(Constraint::equal(
                                arg_info.ty,
                                param_type.clone(),
                                arg_info.span,
                            ));
                        }
                        method_sig.return_type.clone()
                    } else {
                        // Method not found - may be an anonymous struct method
                        // registered during comptime. Use fresh var to avoid ERROR poison.
                        for arg in args.iter() {
                            self.generate(arg.value, ctx);
                        }
                        InferType::Var(self.fresh_var())
                    }
                } else {
                    // Type not found in pre-built context - may be a comptime type var.
                    // Use fresh var so inference doesn't poison surrounding expressions.
                    for arg in args.iter() {
                        self.generate(arg.value, ctx);
                    }
                    InferType::Var(self.fresh_var())
                }
            }

            // Comptime block: the type depends on whether evaluation succeeds at compile time.
            // For type inference, we use a fresh type variable that can unify with
            // whatever type is expected from the context (e.g., a let binding's type annotation).
            // Similar to integer literals, comptime blocks can adapt to their context.
            InstData::Comptime { expr: _ } => {
                // Comptime blocks are fully evaluated by the comptime interpreter in sema,
                // which handles its own type checking (comptime_str, TypeInfo structs, etc.).
                // We don't generate constraints for the inner expression because comptime
                // has types (comptime_str, anonymous structs from @typeInfo) that don't
                // exist in the regular type system and would cause false unification errors.
                // Use a fresh variable so comptime can unify with whatever the context expects.
                let var = self.fresh_var();
                self.int_literal_vars.push(var);
                InferType::Var(var)
            }

            // Comptime unroll for: the iterable is evaluated at comptime, the body is unrolled.
            // Like regular for loops, the result type is unit.
            // We must generate constraints for the body so that type inference resolves
            // types for runtime expressions inside the loop body (e.g., `total + 1`).
            // The binding variable holds a comptime value and is handled by sema, but
            // we register it as an integer type variable so VarRef lookups don't fail.
            InstData::ComptimeUnrollFor {
                binding,
                iterable,
                body,
            } => {
                // Generate constraints for the iterable (it's a comptime block)
                self.generate(*iterable, ctx);

                // Register the binding as a fresh type variable.
                // The actual comptime value type is determined by sema, but HM
                // inference needs the binding in scope so VarRef doesn't fail.
                let binding_ty = {
                    let var = self.fresh_var();
                    InferType::Var(var)
                };
                ctx.insert_local(
                    *binding,
                    LocalVarInfo {
                        ty: binding_ty,
                        is_mut: false,
                        span,
                    },
                );

                // Generate constraints for the body
                self.generate(*body, ctx);

                InferType::Concrete(Type::UNIT)
            }

            // Checked block: for type inference purposes, the type is the type of the inner expression
            // The actual checking of unchecked operations happens in sema
            InstData::Checked { expr } => {
                // Generate constraints for the inner expression
                let inner_info = self.generate(*expr, ctx);
                inner_info.ty
            }

            // Type constant: a type used as a value (e.g., `i32` in `identity(i32, 42)`)
            // This has the special ComptimeType type which indicates it's a type value.
            InstData::TypeConst { .. } => InferType::Concrete(Type::COMPTIME_TYPE),

            // Anonymous struct type: a struct type used as a comptime value
            // This also has the ComptimeType type.
            InstData::AnonStructType { .. } => InferType::Concrete(Type::COMPTIME_TYPE),

            // Anonymous enum type: an enum type used as a comptime value
            // This also has the ComptimeType type.
            InstData::AnonEnumType { .. } => InferType::Concrete(Type::COMPTIME_TYPE),

            // Tuple lowering (ADR-0048): defer to a fresh type variable. The sema
            // layer resolves tuples to anonymous structs during analysis; inference
            // does not need a concrete shape here.
            InstData::TupleInit {
                elems_start,
                elems_len,
            } => {
                let elems = self.rir.get_inst_refs(*elems_start, *elems_len);
                for elem in elems {
                    self.generate(elem, ctx);
                }
                let result_var = self.fresh_var();
                InferType::Var(result_var)
            }

            // Anonymous function value (ADR-0055): sema lowers to a fresh anon
            // struct with a `__call` method, then emits a StructInit against
            // that struct. Inference defers to a fresh type variable here —
            // analysis.rs supplies the concrete struct type.
            InstData::AnonFnValue { .. } => {
                let result_var = self.fresh_var();
                InferType::Var(result_var)
            }
        };

        // Record the type for this expression
        self.record_type(inst_ref, ty.clone());
        ExprInfo::new(ty, span)
    }

    /// Generate constraints for a binary arithmetic operation (+, -, *, /, %).
    ///
    /// Operands must be numeric (integer or float).
    fn generate_binary_arith(
        &mut self,
        lhs: InstRef,
        rhs: InstRef,
        ctx: &mut ConstraintContext,
    ) -> InferType {
        let lhs_info = self.generate(lhs, ctx);
        let rhs_info = self.generate(rhs, ctx);

        let result_var = self.fresh_var();
        let result_ty = InferType::Var(result_var);

        self.add_constraint(Constraint::equal(
            lhs_info.ty,
            result_ty.clone(),
            lhs_info.span,
        ));
        self.add_constraint(Constraint::equal(
            rhs_info.ty,
            result_ty.clone(),
            rhs_info.span,
        ));

        // Result must be numeric (integer or float)
        self.add_constraint(Constraint::is_numeric(result_ty.clone(), lhs_info.span));

        result_ty
    }

    /// Generate constraints for a binary bitwise operation (&, |, ^, <<, >>).
    ///
    /// Operands must be integers (floats are not allowed).
    fn generate_binary_bitwise(
        &mut self,
        lhs: InstRef,
        rhs: InstRef,
        ctx: &mut ConstraintContext,
    ) -> InferType {
        let lhs_info = self.generate(lhs, ctx);
        let rhs_info = self.generate(rhs, ctx);

        let result_var = self.fresh_var();
        let result_ty = InferType::Var(result_var);

        self.add_constraint(Constraint::equal(
            lhs_info.ty,
            result_ty.clone(),
            lhs_info.span,
        ));
        self.add_constraint(Constraint::equal(
            rhs_info.ty,
            result_ty.clone(),
            rhs_info.span,
        ));

        // Result must be an integer type (no floats)
        self.add_constraint(Constraint::is_integer(result_ty.clone(), lhs_info.span));

        result_ty
    }

    /// Get the inferred type for a pattern.
    /// ADR-0051 Phase 4 part 2: register a single data/struct-variant
    /// field binding. If the binding is a flat named binding, insert it
    /// directly. If it carries a nested `sub_pattern`, walk into it via
    /// `collect_recursive_pattern_bindings` so nested Ident leaves
    /// become scope entries.
    fn register_binding(
        &mut self,
        binding: &gruel_rir::RirPatternBinding,
        field_ty: InferType,
        pattern_span: gruel_util::Span,
        ctx: &mut ConstraintContext,
        added_bindings: &mut Vec<(lasso::Spur, Option<LocalVarInfo>)>,
    ) {
        if let Some(sub) = &binding.sub_pattern {
            self.collect_recursive_pattern_bindings(sub, field_ty, ctx, added_bindings);
            return;
        }
        if binding.is_wildcard {
            return;
        }
        if let Some(name) = binding.name {
            let old = ctx.locals.insert(
                name,
                LocalVarInfo {
                    ty: field_ty,
                    is_mut: binding.is_mut,
                    span: pattern_span,
                },
            );
            added_bindings.push((name, old));
        }
    }

    /// ADR-0051 Phase 4c: walk a Tuple / Struct / Ident match-arm
    /// pattern, registering each Ident leaf in `ctx.locals` so body
    /// constraint generation resolves the variable. Field types are
    /// pulled from the concrete struct when `scr_ty` is known; fresh
    /// type variables take over otherwise.
    fn collect_recursive_pattern_bindings(
        &mut self,
        pattern: &gruel_rir::RirPattern,
        scr_ty: InferType,
        ctx: &mut ConstraintContext,
        added_bindings: &mut Vec<(lasso::Spur, Option<LocalVarInfo>)>,
    ) {
        match pattern {
            gruel_rir::RirPattern::Ident { name, is_mut, span } => {
                let old = ctx.locals.insert(
                    *name,
                    LocalVarInfo {
                        ty: scr_ty,
                        is_mut: *is_mut,
                        span: *span,
                    },
                );
                added_bindings.push((*name, old));
            }
            gruel_rir::RirPattern::Tuple { elems, .. } => {
                // Try to resolve scrutinee's struct type to extract field
                // types; fall back to fresh vars when unknown.
                let field_tys: Vec<InferType> = match &scr_ty {
                    InferType::Concrete(ty) => {
                        if let Some(struct_id) = ty.as_struct() {
                            let def = self.type_pool.struct_def(struct_id);
                            def.fields
                                .iter()
                                .map(|f| InferType::Concrete(f.ty))
                                .collect()
                        } else {
                            elems
                                .iter()
                                .map(|_| InferType::Var(self.fresh_var()))
                                .collect()
                        }
                    }
                    _ => elems
                        .iter()
                        .map(|_| InferType::Var(self.fresh_var()))
                        .collect(),
                };
                for (i, elem) in elems.iter().enumerate() {
                    let elem_ty = field_tys
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| InferType::Var(self.fresh_var()));
                    self.collect_recursive_pattern_bindings(elem, elem_ty, ctx, added_bindings);
                }
            }
            gruel_rir::RirPattern::Struct { fields, .. } => {
                // For named-struct roots, we need field types by name.
                // If the scrutinee type is concrete, walk its field list.
                let struct_id_opt = match &scr_ty {
                    InferType::Concrete(ty) => ty.as_struct(),
                    _ => None,
                };
                for rf in fields {
                    let field_ty = if let Some(sid) = struct_id_opt {
                        let def = self.type_pool.struct_def(sid);
                        let name = self.interner.resolve(&rf.field_name);
                        def.fields
                            .iter()
                            .find(|sf| sf.name == name)
                            .map(|sf| InferType::Concrete(sf.ty))
                            .unwrap_or_else(|| InferType::Var(self.fresh_var()))
                    } else {
                        InferType::Var(self.fresh_var())
                    };
                    self.collect_recursive_pattern_bindings(
                        &rf.pattern,
                        field_ty,
                        ctx,
                        added_bindings,
                    );
                }
            }
            gruel_rir::RirPattern::DataVariant {
                type_name,
                bindings,
                ..
            } => {
                // ADR-0052: nested refutable variant sub-pattern.
                // Resolve the enum (if possible) to thread field types
                // through each inner binding.
                let enum_id = self.enums.get(type_name).and_then(|ty| ty.as_enum());
                let field_tys: Vec<InferType> = if let Some(eid) = enum_id
                    && let Some(variant_idx) = self.resolve_variant_index(pattern)
                {
                    let def = self.type_pool.enum_def(eid);
                    def.variants[variant_idx]
                        .fields
                        .iter()
                        .map(|t| InferType::Concrete(*t))
                        .collect()
                } else {
                    bindings
                        .iter()
                        .map(|_| InferType::Var(self.fresh_var()))
                        .collect()
                };
                for (i, binding) in bindings.iter().enumerate() {
                    let ty = field_tys
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| InferType::Var(self.fresh_var()));
                    self.register_binding(binding, ty, pattern.span(), ctx, added_bindings);
                }
            }
            gruel_rir::RirPattern::StructVariant {
                type_name,
                field_bindings,
                ..
            } => {
                let enum_id = self.enums.get(type_name).and_then(|ty| ty.as_enum());
                for fb in field_bindings {
                    let ty = if let Some(eid) = enum_id
                        && let Some(variant_idx) = self.resolve_variant_index(pattern)
                    {
                        let def = self.type_pool.enum_def(eid);
                        let variant_def = &def.variants[variant_idx];
                        let name = self.interner.resolve(&fb.field_name);
                        variant_def
                            .find_field(name)
                            .map(|idx| InferType::Concrete(variant_def.fields[idx]))
                            .unwrap_or_else(|| InferType::Var(self.fresh_var()))
                    } else {
                        InferType::Var(self.fresh_var())
                    };
                    self.register_binding(&fb.binding, ty, pattern.span(), ctx, added_bindings);
                }
            }
            // Leaf literals and Path (unit variant) introduce no
            // additional bindings at this level.
            _ => {}
        }
    }

    /// Helper for `collect_recursive_pattern_bindings`: resolve a
    /// DataVariant / StructVariant's variant index from the RIR shape.
    fn resolve_variant_index(&self, pattern: &gruel_rir::RirPattern) -> Option<usize> {
        let (type_name, variant) = match pattern {
            gruel_rir::RirPattern::DataVariant {
                type_name, variant, ..
            }
            | gruel_rir::RirPattern::StructVariant {
                type_name, variant, ..
            }
            | gruel_rir::RirPattern::Path {
                type_name, variant, ..
            } => (*type_name, *variant),
            _ => return None,
        };
        let enum_id = self.enums.get(&type_name)?.as_enum()?;
        let def = self.type_pool.enum_def(enum_id);
        def.find_variant(self.interner.resolve(&variant))
    }

    fn pattern_type(&mut self, pattern: &gruel_rir::RirPattern) -> InferType {
        match pattern {
            gruel_rir::RirPattern::Wildcard(_) => {
                // Wildcard matches anything - use a fresh type variable
                let var = self.fresh_var();
                InferType::Var(var)
            }
            gruel_rir::RirPattern::Int(_, _) => InferType::IntLiteral,
            gruel_rir::RirPattern::Bool(_, _) => InferType::Concrete(Type::BOOL),
            gruel_rir::RirPattern::Path { type_name, .. }
            | gruel_rir::RirPattern::DataVariant { type_name, .. }
            | gruel_rir::RirPattern::StructVariant { type_name, .. } => {
                if let Some(&enum_ty) = self.enums.get(type_name) {
                    InferType::Concrete(enum_ty)
                } else {
                    // Enum type not found — may be a comptime type variable
                    // (e.g., `let Opt = Option(i32); match x { Opt::Some(v) => ... }`).
                    // Use a fresh type variable so arm bodies can still infer types.
                    let var = self.fresh_var();
                    InferType::Var(var)
                }
            }
            // ADR-0051 Phase 4b: top-level Ident / Tuple / Struct arms
            // don't constrain the scrutinee's type on their own — inference
            // ties them to the scrutinee through a fresh type variable. For
            // Struct arms whose `type_name` resolves to a known struct we
            // could tighten this, but the arm's internal field patterns
            // unify against the scrutinee's struct type elsewhere.
            gruel_rir::RirPattern::Ident { .. }
            | gruel_rir::RirPattern::Tuple { .. }
            | gruel_rir::RirPattern::Struct { .. } => {
                let var = self.fresh_var();
                InferType::Var(var)
            }
        }
    }

    /// Resolve a type name to an InferType.
    ///
    /// Handles primitive types, array syntax `[T; N]`, pointer syntax `ptr mut T` / `ptr const T`,
    /// and struct/enum types.
    fn resolve_type_name(&self, name: &str) -> Option<InferType> {
        // Check for array syntax first: [T; N]
        if let Some((element_type_str, length)) = parse_array_type_syntax(name) {
            // Recursively resolve the element type
            let element_ty = self.resolve_type_name(&element_type_str)?;
            return Some(InferType::Array {
                element: Box::new(element_ty),
                length,
            });
        }

        // ADR-0061: built-in parameterized types (`Ptr(T)`, `MutPtr(T)`).
        if let Some((callee_name, arg_strs)) = parse_type_call_syntax(name)
            && let Some(constructor) = gruel_builtins::get_builtin_type_constructor(&callee_name)
            && arg_strs.len() == constructor.arity
        {
            let arg_infer = self.resolve_type_name(&arg_strs[0])?;
            let arg_ty = match arg_infer {
                InferType::Concrete(ty) => ty,
                _ => return None,
            };
            let ptr_ty = match constructor.kind {
                BuiltinTypeConstructorKind::Ptr => {
                    let id = self.type_pool.intern_ptr_const_from_type(arg_ty);
                    Type::new_ptr_const(id)
                }
                BuiltinTypeConstructorKind::MutPtr => {
                    let id = self.type_pool.intern_ptr_mut_from_type(arg_ty);
                    Type::new_ptr_mut(id)
                }
                BuiltinTypeConstructorKind::Ref => {
                    let id = self.type_pool.intern_ref_from_type(arg_ty);
                    Type::new_ref(id)
                }
                BuiltinTypeConstructorKind::MutRef => {
                    let id = self.type_pool.intern_mut_ref_from_type(arg_ty);
                    Type::new_mut_ref(id)
                }
                BuiltinTypeConstructorKind::Slice => {
                    let id = self.type_pool.intern_slice_from_type(arg_ty);
                    Type::new_slice(id)
                }
                BuiltinTypeConstructorKind::MutSlice => {
                    let id = self.type_pool.intern_mut_slice_from_type(arg_ty);
                    Type::new_mut_slice(id)
                }
                BuiltinTypeConstructorKind::Vec => {
                    let id = self.type_pool.intern_vec_from_type(arg_ty);
                    Type::new_vec(id)
                }
            };
            return Some(InferType::Concrete(ptr_ty));
        }

        // Check for pointer syntax: ptr mut T / ptr const T
        if let Some((pointee_type_str, mutability)) = parse_pointer_type_syntax(name) {
            // Recursively resolve the pointee type
            let pointee_infer_ty = self.resolve_type_name(&pointee_type_str)?;

            // Convert InferType to Type so we can intern the pointer
            let pointee_ty = match pointee_infer_ty {
                InferType::Concrete(ty) => ty,
                // Can't handle non-concrete types in pointer positions during constraint generation
                _ => return None,
            };

            // Intern the pointer type
            let ptr_ty = match mutability {
                PtrMutability::Mut => {
                    let ptr_id = self.type_pool.intern_ptr_mut_from_type(pointee_ty);
                    Type::new_ptr_mut(ptr_id)
                }
                PtrMutability::Const => {
                    let ptr_id = self.type_pool.intern_ptr_const_from_type(pointee_ty);
                    Type::new_ptr_const(ptr_id)
                }
            };
            return Some(InferType::Concrete(ptr_ty));
        }

        // Check primitives
        let ty = match name {
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
            "()" => Type::UNIT,
            _ => {
                // Check for struct types (including builtin String)
                if let Some(name_spur) = self.interner.get(name) {
                    if let Some(&struct_ty) = self.structs.get(&name_spur) {
                        return Some(InferType::Concrete(struct_ty));
                    }
                    if let Some(&enum_ty) = self.enums.get(&name_spur) {
                        return Some(InferType::Concrete(enum_ty));
                    }
                }
                return None;
            }
        };
        Some(InferType::Concrete(ty))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lasso::ThreadedRodeo;

    /// Helper to create a minimal RIR, interner, and type pool for testing.
    fn make_test_rir_interner_and_type_pool() -> (Rir, ThreadedRodeo, TypeInternPool) {
        let rir = Rir::new();
        let interner = ThreadedRodeo::new();
        let type_pool = TypeInternPool::new();
        (rir, interner, type_pool)
    }

    #[test]
    fn test_constraint_generator_int_literal() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::default();
        let structs = HashMap::default();
        let enums = HashMap::default();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::default();
        let enum_methods: HashMap<(EnumId, Spur), MethodSig> = HashMap::default();

        // Add an integer constant to RIR
        let inst_ref = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let infer_ctx = InferenceContext {
            func_sigs: functions.clone(),
            struct_types: structs.clone(),
            enum_types: enums.clone(),
            method_sigs: methods.clone(),
            enum_method_sigs: enum_methods.clone(),
        };
        let mut cgen = ConstraintGenerator::new(&rir, &interner, &infer_ctx, &type_pool);
        let params = HashMap::default();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(inst_ref, &mut ctx);

        // Integer literals now get a type variable (tracked as int literal var)
        assert!(matches!(info.ty, InferType::Var(_)));
        // The type variable should be tracked in int_literal_vars
        assert_eq!(cgen.int_literal_vars().len(), 1);
        // No constraints should be generated for a simple literal
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_bool_literal() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::default();
        let structs = HashMap::default();
        let enums = HashMap::default();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::default();
        let enum_methods: HashMap<(EnumId, Spur), MethodSig> = HashMap::default();

        let inst_ref = rir.add_inst(gruel_rir::Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });

        let infer_ctx = InferenceContext {
            func_sigs: functions.clone(),
            struct_types: structs.clone(),
            enum_types: enums.clone(),
            method_sigs: methods.clone(),
            enum_method_sigs: enum_methods.clone(),
        };
        let mut cgen = ConstraintGenerator::new(&rir, &interner, &infer_ctx, &type_pool);
        let params = HashMap::default();
        let mut ctx = ConstraintContext::new(&params, Type::BOOL);

        let info = cgen.generate(inst_ref, &mut ctx);

        assert_eq!(info.ty, InferType::Concrete(Type::BOOL));
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_binary_add() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::default();
        let structs = HashMap::default();
        let enums = HashMap::default();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::default();
        let enum_methods: HashMap<(EnumId, Spur), MethodSig> = HashMap::default();

        // Create: 1 + 2
        let lhs = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let rhs = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(2),
            span: Span::new(4, 5),
        });
        let add = rir.add_inst(gruel_rir::Inst {
            data: InstData::Bin {
                op: BinOp::Add,
                lhs,
                rhs,
            },
            span: Span::new(0, 5),
        });

        let infer_ctx = InferenceContext {
            func_sigs: functions.clone(),
            struct_types: structs.clone(),
            enum_types: enums.clone(),
            method_sigs: methods.clone(),
            enum_method_sigs: enum_methods.clone(),
        };
        let mut cgen = ConstraintGenerator::new(&rir, &interner, &infer_ctx, &type_pool);
        let params = HashMap::default();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(add, &mut ctx);

        // Result should be a type variable
        assert!(info.ty.is_var());
        // Should generate 3 constraints: lhs = result, rhs = result, IsNumeric(result)
        assert_eq!(cgen.constraints().len(), 3);
        // Verify the third constraint is IsNumeric
        match &cgen.constraints()[2] {
            Constraint::IsNumeric(_, _) => {}
            _ => panic!("Expected IsNumeric constraint for arithmetic result"),
        }
    }

    #[test]
    fn test_constraint_generator_comparison() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::default();
        let structs = HashMap::default();
        let enums = HashMap::default();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::default();
        let enum_methods: HashMap<(EnumId, Spur), MethodSig> = HashMap::default();

        // Create: 1 < 2
        let lhs = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let rhs = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(2),
            span: Span::new(4, 5),
        });
        let lt = rir.add_inst(gruel_rir::Inst {
            data: InstData::Bin {
                op: BinOp::Lt,
                lhs,
                rhs,
            },
            span: Span::new(0, 5),
        });

        let infer_ctx = InferenceContext {
            func_sigs: functions.clone(),
            struct_types: structs.clone(),
            enum_types: enums.clone(),
            method_sigs: methods.clone(),
            enum_method_sigs: enum_methods.clone(),
        };
        let mut cgen = ConstraintGenerator::new(&rir, &interner, &infer_ctx, &type_pool);
        let params = HashMap::default();
        let mut ctx = ConstraintContext::new(&params, Type::BOOL);

        let info = cgen.generate(lt, &mut ctx);

        // Comparisons always return Bool
        assert_eq!(info.ty, InferType::Concrete(Type::BOOL));
        // Should generate 1 constraint: lhs type = rhs type
        assert_eq!(cgen.constraints().len(), 1);
    }

    #[test]
    fn test_constraint_generator_logical_and() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::default();
        let structs = HashMap::default();
        let enums = HashMap::default();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::default();
        let enum_methods: HashMap<(EnumId, Spur), MethodSig> = HashMap::default();

        // Create: true && false
        let lhs = rir.add_inst(gruel_rir::Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });
        let rhs = rir.add_inst(gruel_rir::Inst {
            data: InstData::BoolConst(false),
            span: Span::new(8, 13),
        });
        let and = rir.add_inst(gruel_rir::Inst {
            data: InstData::Bin {
                op: BinOp::And,
                lhs,
                rhs,
            },
            span: Span::new(0, 13),
        });

        let infer_ctx = InferenceContext {
            func_sigs: functions.clone(),
            struct_types: structs.clone(),
            enum_types: enums.clone(),
            method_sigs: methods.clone(),
            enum_method_sigs: enum_methods.clone(),
        };
        let mut cgen = ConstraintGenerator::new(&rir, &interner, &infer_ctx, &type_pool);
        let params = HashMap::default();
        let mut ctx = ConstraintContext::new(&params, Type::BOOL);

        let info = cgen.generate(and, &mut ctx);

        // Logical operators return Bool
        assert_eq!(info.ty, InferType::Concrete(Type::BOOL));
        // Should generate 2 constraints: lhs = bool, rhs = bool
        assert_eq!(cgen.constraints().len(), 2);
    }

    #[test]
    fn test_constraint_generator_negation() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::default();
        let structs = HashMap::default();
        let enums = HashMap::default();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::default();
        let enum_methods: HashMap<(EnumId, Spur), MethodSig> = HashMap::default();

        // Create: -42
        let operand = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(1, 3),
        });
        let neg = rir.add_inst(gruel_rir::Inst {
            data: InstData::Unary {
                op: UnaryOp::Neg,
                operand,
            },
            span: Span::new(0, 3),
        });

        let infer_ctx = InferenceContext {
            func_sigs: functions.clone(),
            struct_types: structs.clone(),
            enum_types: enums.clone(),
            method_sigs: methods.clone(),
            enum_method_sigs: enum_methods.clone(),
        };
        let mut cgen = ConstraintGenerator::new(&rir, &interner, &infer_ctx, &type_pool);
        let params = HashMap::default();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(neg, &mut ctx);

        // Negation preserves the operand type (now a type variable for the int literal)
        assert!(matches!(info.ty, InferType::Var(_)));
        // Should generate 1 constraint: IsSigned for the result
        assert_eq!(cgen.constraints().len(), 1);
        // Verify it's an IsSigned constraint
        match &cgen.constraints()[0] {
            Constraint::IsSigned(_, _) => {}
            _ => panic!("Expected IsSigned constraint"),
        }
    }

    #[test]
    fn test_constraint_generator_return() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::default();
        let structs = HashMap::default();
        let enums = HashMap::default();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::default();
        let enum_methods: HashMap<(EnumId, Spur), MethodSig> = HashMap::default();

        // Create: return 42
        let value = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(7, 9),
        });
        let ret = rir.add_inst(gruel_rir::Inst {
            data: InstData::Ret(Some(value)),
            span: Span::new(0, 9),
        });

        let infer_ctx = InferenceContext {
            func_sigs: functions.clone(),
            struct_types: structs.clone(),
            enum_types: enums.clone(),
            method_sigs: methods.clone(),
            enum_method_sigs: enum_methods.clone(),
        };
        let mut cgen = ConstraintGenerator::new(&rir, &interner, &infer_ctx, &type_pool);
        let params = HashMap::default();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(ret, &mut ctx);

        // Return is divergent (Never type)
        assert_eq!(info.ty, InferType::Concrete(Type::NEVER));
        // Should generate 1 constraint: return value = return type
        assert_eq!(cgen.constraints().len(), 1);
    }

    #[test]
    fn test_constraint_generator_if_else() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::default();
        let structs = HashMap::default();
        let enums = HashMap::default();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::default();
        let enum_methods: HashMap<(EnumId, Spur), MethodSig> = HashMap::default();

        // Create: if true { 1 } else { 2 }
        let cond = rir.add_inst(gruel_rir::Inst {
            data: InstData::BoolConst(true),
            span: Span::new(3, 7),
        });
        let then_val = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(10, 11),
        });
        let else_val = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(2),
            span: Span::new(20, 21),
        });
        let branch = rir.add_inst(gruel_rir::Inst {
            data: InstData::Branch {
                cond,
                then_block: then_val,
                else_block: Some(else_val),
            },
            span: Span::new(0, 25),
        });

        let infer_ctx = InferenceContext {
            func_sigs: functions.clone(),
            struct_types: structs.clone(),
            enum_types: enums.clone(),
            method_sigs: methods.clone(),
            enum_method_sigs: enum_methods.clone(),
        };
        let mut cgen = ConstraintGenerator::new(&rir, &interner, &infer_ctx, &type_pool);
        let params = HashMap::default();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(branch, &mut ctx);

        // Result should be a type variable (unified from both branches)
        assert!(info.ty.is_var());
        // Should generate 3 constraints: cond = bool, then = result, else = result
        assert_eq!(cgen.constraints().len(), 3);
    }

    #[test]
    fn test_constraint_generator_while_loop() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::default();
        let structs = HashMap::default();
        let enums = HashMap::default();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::default();
        let enum_methods: HashMap<(EnumId, Spur), MethodSig> = HashMap::default();

        // Create: while true { 0 }
        let cond = rir.add_inst(gruel_rir::Inst {
            data: InstData::BoolConst(true),
            span: Span::new(6, 10),
        });
        let body = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(0),
            span: Span::new(13, 14),
        });
        let loop_inst = rir.add_inst(gruel_rir::Inst {
            data: InstData::Loop { cond, body },
            span: Span::new(0, 15),
        });

        let infer_ctx = InferenceContext {
            func_sigs: functions.clone(),
            struct_types: structs.clone(),
            enum_types: enums.clone(),
            method_sigs: methods.clone(),
            enum_method_sigs: enum_methods.clone(),
        };
        let mut cgen = ConstraintGenerator::new(&rir, &interner, &infer_ctx, &type_pool);
        let params = HashMap::default();
        let mut ctx = ConstraintContext::new(&params, Type::UNIT);

        let info = cgen.generate(loop_inst, &mut ctx);

        // While loops produce Unit
        assert_eq!(info.ty, InferType::Concrete(Type::UNIT));
        // Should generate 1 constraint: cond = bool
        assert_eq!(cgen.constraints().len(), 1);
    }

    #[test]
    fn test_constraint_context_scope() {
        let params = HashMap::default();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        // Use an interner to create a symbol
        let interner = ThreadedRodeo::new();
        let sym = interner.get_or_intern("x");
        ctx.insert_local(
            sym,
            LocalVarInfo {
                ty: InferType::Concrete(Type::I32),
                is_mut: false,
                span: Span::new(0, 1),
            },
        );

        assert!(ctx.locals.contains_key(&sym));

        // Push a scope and shadow the variable
        ctx.push_scope();
        ctx.insert_local(
            sym,
            LocalVarInfo {
                ty: InferType::Concrete(Type::I64),
                is_mut: true,
                span: Span::new(10, 15),
            },
        );

        // Should see the shadowed version
        let local = ctx.locals.get(&sym).unwrap();
        assert_eq!(local.ty, InferType::Concrete(Type::I64));
        assert!(local.is_mut);

        // Pop scope - should restore original
        ctx.pop_scope();
        let local = ctx.locals.get(&sym).unwrap();
        assert_eq!(local.ty, InferType::Concrete(Type::I32));
        assert!(!local.is_mut);
    }

    #[test]
    fn test_expr_info_creation() {
        let info = ExprInfo::new(InferType::IntLiteral, Span::new(5, 10));
        assert!(info.ty.is_int_literal());
        assert_eq!(info.span, Span::new(5, 10));
    }

    /// Helper to create a non-generic FunctionSig for tests
    fn make_test_func_sig(param_types: Vec<InferType>, return_type: InferType) -> FunctionSig {
        let num_params = param_types.len();
        FunctionSig {
            param_types,
            return_type,
            is_generic: false,
            param_modes: vec![gruel_rir::RirParamMode::Normal; num_params],
            param_comptime: vec![false; num_params],
            param_names: vec![],
            return_type_sym: lasso::Spur::default(),
        }
    }

    #[test]
    fn test_function_sig() {
        let sig = make_test_func_sig(
            vec![
                InferType::Concrete(Type::I32),
                InferType::Concrete(Type::BOOL),
            ],
            InferType::Concrete(Type::I64),
        );
        assert_eq!(sig.param_types.len(), 2);
        assert_eq!(sig.return_type, InferType::Concrete(Type::I64));
    }

    #[test]
    fn test_constraint_generator_infinite_loop() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::default();
        let structs = HashMap::default();
        let enums = HashMap::default();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::default();
        let enum_methods: HashMap<(EnumId, Spur), MethodSig> = HashMap::default();

        // Create: loop { 0 }
        let body = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(0),
            span: Span::new(6, 7),
        });
        let loop_inst = rir.add_inst(gruel_rir::Inst {
            data: InstData::InfiniteLoop { body },
            span: Span::new(0, 10),
        });

        let infer_ctx = InferenceContext {
            func_sigs: functions.clone(),
            struct_types: structs.clone(),
            enum_types: enums.clone(),
            method_sigs: methods.clone(),
            enum_method_sigs: enum_methods.clone(),
        };
        let mut cgen = ConstraintGenerator::new(&rir, &interner, &infer_ctx, &type_pool);
        let params = HashMap::default();
        let mut ctx = ConstraintContext::new(&params, Type::UNIT);

        let info = cgen.generate(loop_inst, &mut ctx);

        // Infinite loop produces Never (diverges)
        assert_eq!(info.ty, InferType::Concrete(Type::NEVER));
        // No constraints for infinite loop itself
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_break_continue() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::default();
        let structs = HashMap::default();
        let enums = HashMap::default();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::default();
        let enum_methods: HashMap<(EnumId, Spur), MethodSig> = HashMap::default();

        let break_inst = rir.add_inst(gruel_rir::Inst {
            data: InstData::Break,
            span: Span::new(0, 5),
        });

        let infer_ctx = InferenceContext {
            func_sigs: functions.clone(),
            struct_types: structs.clone(),
            enum_types: enums.clone(),
            method_sigs: methods.clone(),
            enum_method_sigs: enum_methods.clone(),
        };
        let mut cgen = ConstraintGenerator::new(&rir, &interner, &infer_ctx, &type_pool);
        let params = HashMap::default();
        let mut ctx = ConstraintContext::new(&params, Type::UNIT);

        let info = cgen.generate(break_inst, &mut ctx);

        // Break diverges
        assert_eq!(info.ty, InferType::Concrete(Type::NEVER));
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_index_get() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::default();
        let structs = HashMap::default();
        let enums = HashMap::default();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::default();
        let enum_methods: HashMap<(EnumId, Spur), MethodSig> = HashMap::default();

        // Create: arr[0]
        let base = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(0), // Placeholder for array
            span: Span::new(0, 3),
        });
        let index = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(0),
            span: Span::new(4, 5),
        });
        let index_get = rir.add_inst(gruel_rir::Inst {
            data: InstData::IndexGet { base, index },
            span: Span::new(0, 6),
        });

        let infer_ctx = InferenceContext {
            func_sigs: functions.clone(),
            struct_types: structs.clone(),
            enum_types: enums.clone(),
            method_sigs: methods.clone(),
            enum_method_sigs: enum_methods.clone(),
        };
        let mut cgen = ConstraintGenerator::new(&rir, &interner, &infer_ctx, &type_pool);
        let params = HashMap::default();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(index_get, &mut ctx);

        // Result is a type variable (element type unknown)
        assert!(info.ty.is_var());
        // Should generate 1 constraint: index must be `usize`
        assert_eq!(cgen.constraints().len(), 1);
        match &cgen.constraints()[0] {
            Constraint::Equal(lhs, _rhs, _) => {
                assert_eq!(*lhs, InferType::Concrete(Type::USIZE));
            }
            _ => panic!("Expected Equal(USIZE, _) constraint for index"),
        }
    }

    #[test]
    fn test_constraint_generator_index_set() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::default();
        let structs = HashMap::default();
        let enums = HashMap::default();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::default();
        let enum_methods: HashMap<(EnumId, Spur), MethodSig> = HashMap::default();

        // Create: arr[0] = 42
        let base = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(0), // Placeholder for array
            span: Span::new(0, 3),
        });
        let index = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(0),
            span: Span::new(4, 5),
        });
        let value = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(9, 11),
        });
        let index_set = rir.add_inst(gruel_rir::Inst {
            data: InstData::IndexSet { base, index, value },
            span: Span::new(0, 11),
        });

        let infer_ctx = InferenceContext {
            func_sigs: functions.clone(),
            struct_types: structs.clone(),
            enum_types: enums.clone(),
            method_sigs: methods.clone(),
            enum_method_sigs: enum_methods.clone(),
        };
        let mut cgen = ConstraintGenerator::new(&rir, &interner, &infer_ctx, &type_pool);
        let params = HashMap::default();
        let mut ctx = ConstraintContext::new(&params, Type::UNIT);

        let info = cgen.generate(index_set, &mut ctx);

        // Index assignment produces Unit
        assert_eq!(info.ty, InferType::Concrete(Type::UNIT));
        // Should generate 1 constraint: index must be `usize`
        assert_eq!(cgen.constraints().len(), 1);
        match &cgen.constraints()[0] {
            Constraint::Equal(lhs, _rhs, _) => {
                assert_eq!(*lhs, InferType::Concrete(Type::USIZE));
            }
            _ => panic!("Expected Equal(USIZE, _) constraint for index"),
        }
    }

    #[test]
    fn test_constraint_generator_empty_block() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::default();
        let structs = HashMap::default();
        let enums = HashMap::default();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::default();
        let enum_methods: HashMap<(EnumId, Spur), MethodSig> = HashMap::default();

        // Create: { } (empty block)
        let block = rir.add_inst(gruel_rir::Inst {
            data: InstData::Block {
                extra_start: 0,
                len: 0,
            },
            span: Span::new(0, 2),
        });

        let infer_ctx = InferenceContext {
            func_sigs: functions.clone(),
            struct_types: structs.clone(),
            enum_types: enums.clone(),
            method_sigs: methods.clone(),
            enum_method_sigs: enum_methods.clone(),
        };
        let mut cgen = ConstraintGenerator::new(&rir, &interner, &infer_ctx, &type_pool);
        let params = HashMap::default();
        let mut ctx = ConstraintContext::new(&params, Type::UNIT);

        let info = cgen.generate(block, &mut ctx);

        // Empty block produces Unit
        assert_eq!(info.ty, InferType::Concrete(Type::UNIT));
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_bitwise_not() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::default();
        let structs = HashMap::default();
        let enums = HashMap::default();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::default();
        let enum_methods: HashMap<(EnumId, Spur), MethodSig> = HashMap::default();

        // Create: !42 (bitwise NOT)
        let operand = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(1, 3),
        });
        let bitnot = rir.add_inst(gruel_rir::Inst {
            data: InstData::Unary {
                op: UnaryOp::BitNot,
                operand,
            },
            span: Span::new(0, 3),
        });

        let infer_ctx = InferenceContext {
            func_sigs: functions.clone(),
            struct_types: structs.clone(),
            enum_types: enums.clone(),
            method_sigs: methods.clone(),
            enum_method_sigs: enum_methods.clone(),
        };
        let mut cgen = ConstraintGenerator::new(&rir, &interner, &infer_ctx, &type_pool);
        let params = HashMap::default();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(bitnot, &mut ctx);

        // Bitwise NOT preserves the operand type (now a type variable for int literal)
        assert!(matches!(info.ty, InferType::Var(_)));
        // Should generate 1 constraint: IsInteger for the result
        assert_eq!(cgen.constraints().len(), 1);
        match &cgen.constraints()[0] {
            Constraint::IsInteger(_, _) => {}
            _ => panic!("Expected IsInteger constraint"),
        }
    }

    #[test]
    fn test_constraint_generator_function_call_arg_count_mismatch() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let mut functions = HashMap::default();
        let structs = HashMap::default();
        let enums = HashMap::default();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::default();
        let enum_methods: HashMap<(EnumId, Spur), MethodSig> = HashMap::default();

        // Register a function that takes 2 parameters
        let func_name = interner.get_or_intern("foo");
        functions.insert(
            func_name,
            make_test_func_sig(
                vec![
                    InferType::Concrete(Type::I32),
                    InferType::Concrete(Type::I32),
                ],
                InferType::Concrete(Type::BOOL),
            ),
        );

        // Create a call with only 1 argument (mismatch)
        let arg = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(4, 6),
        });
        let (args_start, args_len) = rir.add_call_args(&[gruel_rir::RirCallArg {
            value: arg,
            mode: gruel_rir::RirArgMode::Normal,
        }]);
        let call = rir.add_inst(gruel_rir::Inst {
            data: InstData::Call {
                name: func_name,
                args_start,
                args_len,
            },
            span: Span::new(0, 7),
        });

        let infer_ctx = InferenceContext {
            func_sigs: functions.clone(),
            struct_types: structs.clone(),
            enum_types: enums.clone(),
            method_sigs: methods.clone(),
            enum_method_sigs: enum_methods.clone(),
        };
        let mut cgen = ConstraintGenerator::new(&rir, &interner, &infer_ctx, &type_pool);
        let params = HashMap::default();
        let mut ctx = ConstraintContext::new(&params, Type::BOOL);

        let info = cgen.generate(call, &mut ctx);

        // Should still return the declared return type
        assert_eq!(info.ty, InferType::Concrete(Type::BOOL));
        // No constraints generated when arg count mismatches (error will be in sema)
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_unknown_function() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::default(); // Empty - no functions registered
        let structs = HashMap::default();
        let enums = HashMap::default();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::default();
        let enum_methods: HashMap<(EnumId, Spur), MethodSig> = HashMap::default();

        // Create a call to an unknown function
        let unknown_func = interner.get_or_intern("unknown");
        let arg = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(8, 10),
        });
        let (args_start, args_len) = rir.add_call_args(&[gruel_rir::RirCallArg {
            value: arg,
            mode: gruel_rir::RirArgMode::Normal,
        }]);
        let call = rir.add_inst(gruel_rir::Inst {
            data: InstData::Call {
                name: unknown_func,
                args_start,
                args_len,
            },
            span: Span::new(0, 11),
        });

        let infer_ctx = InferenceContext {
            func_sigs: functions.clone(),
            struct_types: structs.clone(),
            enum_types: enums.clone(),
            method_sigs: methods.clone(),
            enum_method_sigs: enum_methods.clone(),
        };
        let mut cgen = ConstraintGenerator::new(&rir, &interner, &infer_ctx, &type_pool);
        let params = HashMap::default();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(call, &mut ctx);

        // Unknown function returns Error type
        assert_eq!(info.ty, InferType::Concrete(Type::ERROR));
        // Arguments should still be processed (but no constraints generated for them)
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_match_multiple_arms() {
        let (mut rir, interner, type_pool) = make_test_rir_interner_and_type_pool();
        let functions = HashMap::default();
        let structs = HashMap::default();
        let enums = HashMap::default();
        let methods: HashMap<(StructId, Spur), MethodSig> = HashMap::default();
        let enum_methods: HashMap<(EnumId, Spur), MethodSig> = HashMap::default();

        // Create: match x { 1 => 10, 2 => 20, _ => 30 }
        let scrutinee = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(5),
            span: Span::new(6, 7),
        });

        // Arm 1: 1 => 10
        let body1 = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(10),
            span: Span::new(15, 17),
        });
        let pattern1 = gruel_rir::RirPattern::Int(1, Span::new(10, 11));

        // Arm 2: 2 => 20
        let body2 = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(20),
            span: Span::new(25, 27),
        });
        let pattern2 = gruel_rir::RirPattern::Int(2, Span::new(20, 21));

        // Arm 3: _ => 30
        let body3 = rir.add_inst(gruel_rir::Inst {
            data: InstData::IntConst(30),
            span: Span::new(35, 37),
        });
        let pattern3 = gruel_rir::RirPattern::Wildcard(Span::new(30, 31));

        let arms = vec![(pattern1, body1), (pattern2, body2), (pattern3, body3)];
        let (arms_start, arms_len) = rir.add_match_arms(&arms);
        let match_inst = rir.add_inst(gruel_rir::Inst {
            data: InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            },
            span: Span::new(0, 40),
        });

        let infer_ctx = InferenceContext {
            func_sigs: functions.clone(),
            struct_types: structs.clone(),
            enum_types: enums.clone(),
            method_sigs: methods.clone(),
            enum_method_sigs: enum_methods.clone(),
        };
        let mut cgen = ConstraintGenerator::new(&rir, &interner, &infer_ctx, &type_pool);
        let params = HashMap::default();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(match_inst, &mut ctx);

        // Result should be a type variable (unified from all arm bodies)
        assert!(info.ty.is_var());

        // Should generate 6 constraints:
        // - 3 for pattern types matching scrutinee type (each arm)
        // - 3 for body types matching result type (each arm)
        assert_eq!(cgen.constraints().len(), 6);

        // Verify all constraints are Equal constraints
        for constraint in cgen.constraints() {
            match constraint {
                Constraint::Equal(_, _, _) => {}
                _ => panic!("Expected Equal constraint in match"),
            }
        }
    }
}

//! Pointer, char, and slice operation lowering for the semantic-analysis pass.
//!
//! Hosts the analyzers for raw-pointer intrinsics (`@ptr_read`,
//! `@ptr_write`, `@ptr_offset`, etc.), the dispatch helpers for pointer
//! / char / slice associated-functions and methods, and the shared
//! `lower_pointer_op_to_air` helper that turns a pointer-method call
//! into the corresponding AIR instruction.

use gruel_intrinsics::{IntrinsicId, PointerKind, PointerOpForm, lookup_pointer_method};
use gruel_rir::{InstRef, RirCallArg};
use gruel_util::{CompileError, CompileResult, ErrorKind, IntrinsicTypeMismatchError, Span};
use lasso::Spur;

use super::Sema;
use super::context::{AnalysisContext, AnalysisResult};
use crate::inst::{Air, AirArgMode, AirInst, AirInstData, AirRef};
use crate::types::{Type, TypeKind};

/// Identifies a pointer-method intrinsic in the registry by its compiler-side
/// `IntrinsicId`, the interned symbol used at the call site, and the
/// human-readable operator name (e.g. `"read"`, `"add"`) used in error
/// messages. All three travel together.
struct PointerOpKind<'a> {
    intrinsic: IntrinsicId,
    name: Spur,
    op_name: &'a str,
}

/// Origin of the pointer value for a pointer-op lowering: either a
/// receiver from a method call (`Some(_), None`) or a left-hand-side type
/// from an associated-function call (`None, Some(_)`).
struct PointerOpOrigin {
    receiver: Option<AnalysisResult>,
    lhs_type: Option<Type>,
}

fn pointer_kind_name(kind: PointerKind) -> &'static str {
    match kind {
        PointerKind::Ptr => "Ptr",
        PointerKind::MutPtr => "MutPtr",
    }
}

impl<'a> Sema<'a> {
    /// Analyze @ptr_read intrinsic: reads value through pointer.
    /// Signature: @ptr_read(ptr: ptr const T) -> T
    pub(super) fn analyze_ptr_read_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "ptr_read".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        let ptr_result = self.analyze_inst(air, args[0].value, ctx)?;
        let ptr_type = ptr_result.ty;

        // Get the pointee type from the pointer type
        let pointee_type = match ptr_type.kind() {
            TypeKind::PtrConst(ptr_id) => self.type_pool.ptr_const_def(ptr_id),
            TypeKind::PtrMut(ptr_id) => self.type_pool.ptr_mut_def(ptr_id),
            _ => {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: "ptr_read".to_string(),
                        expected: "Ptr(T) or MutPtr(T)".to_string(),
                        found: self.format_type_name(ptr_type),
                    })),
                    span,
                ));
            }
        };

        // Create the intrinsic call instruction
        let args_start = air.add_extra(&[ptr_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 1,
            },
            ty: pointee_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, pointee_type))
    }

    /// Analyze @ptr_write intrinsic: writes value through pointer.
    /// Signature: @ptr_write(ptr: ptr mut T, value: T) -> ()
    pub(super) fn analyze_ptr_write_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 2 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "ptr_write".to_string(),
                    expected: 2,
                    found: args.len(),
                },
                span,
            ));
        }

        let ptr_result = self.analyze_inst(air, args[0].value, ctx)?;
        let value_result = self.analyze_inst(air, args[1].value, ctx)?;
        let ptr_type = ptr_result.ty;
        let value_type = value_result.ty;

        // Pointer must be ptr mut T
        let pointee_type = match ptr_type.kind() {
            TypeKind::PtrMut(ptr_id) => self.type_pool.ptr_mut_def(ptr_id),
            TypeKind::PtrConst(_) => {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: "ptr_write".to_string(),
                        expected: "MutPtr(T) (cannot write through Ptr)".to_string(),
                        found: self.format_type_name(ptr_type),
                    })),
                    span,
                ));
            }
            _ => {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: "ptr_write".to_string(),
                        expected: "MutPtr(T)".to_string(),
                        found: self.format_type_name(ptr_type),
                    })),
                    span,
                ));
            }
        };

        // Check that value type matches pointee type
        if value_type != pointee_type && !value_type.is_error() && !value_type.is_never() {
            return Err(CompileError::type_mismatch(
                self.format_type_name(pointee_type),
                self.format_type_name(value_type),
                span,
            ));
        }

        // Create the intrinsic call instruction
        let args_start =
            air.add_extra(&[ptr_result.air_ref.as_u32(), value_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 2,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
    }

    /// Analyze @ptr_offset intrinsic: pointer arithmetic.
    /// Signature: @ptr_offset(ptr: ptr T, offset: i64) -> ptr T
    pub(super) fn analyze_ptr_offset_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 2 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "ptr_offset".to_string(),
                    expected: 2,
                    found: args.len(),
                },
                span,
            ));
        }

        let ptr_result = self.analyze_inst(air, args[0].value, ctx)?;
        let offset_result = self.analyze_inst(air, args[1].value, ctx)?;
        let ptr_type = ptr_result.ty;
        let offset_type = offset_result.ty;

        // Validate pointer type
        if !ptr_type.is_ptr() && !ptr_type.is_error() && !ptr_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "ptr_offset".to_string(),
                    expected: "Ptr(T) or MutPtr(T)".to_string(),
                    found: self.format_type_name(ptr_type),
                })),
                span,
            ));
        }

        // Validate offset type (must be integer)
        if !offset_type.is_integer() && !offset_type.is_error() && !offset_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "ptr_offset".to_string(),
                    expected: "integer offset".to_string(),
                    found: self.format_type_name(offset_type),
                })),
                span,
            ));
        }

        // Create the intrinsic call instruction (returns same pointer type)
        let args_start =
            air.add_extra(&[ptr_result.air_ref.as_u32(), offset_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 2,
            },
            ty: ptr_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, ptr_type))
    }

    /// Analyze @ptr_to_int intrinsic: converts pointer to u64.
    /// Signature: @ptr_to_int(ptr: ptr T) -> u64
    pub(super) fn analyze_ptr_to_int_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "ptr_to_int".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        let ptr_result = self.analyze_inst(air, args[0].value, ctx)?;
        let ptr_type = ptr_result.ty;

        // Validate pointer type
        if !ptr_type.is_ptr() && !ptr_type.is_error() && !ptr_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "ptr_to_int".to_string(),
                    expected: "Ptr(T) or MutPtr(T)".to_string(),
                    found: self.format_type_name(ptr_type),
                })),
                span,
            ));
        }

        // Create the intrinsic call instruction (returns u64)
        let args_start = air.add_extra(&[ptr_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 1,
            },
            ty: Type::U64,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::U64))
    }

    /// Analyze @int_to_ptr intrinsic: converts u64 to pointer.
    /// Signature: @int_to_ptr(addr: u64) -> ptr mut T
    /// The result type T is inferred from context (e.g., `let p: ptr mut i32 = @int_to_ptr(addr)`)
    pub(super) fn analyze_int_to_ptr_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "int_to_ptr".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        let addr_result = self.analyze_inst(air, args[0].value, ctx)?;
        let addr_type = addr_result.ty;

        // Validate address type (must be u64)
        if addr_type != Type::U64 && !addr_type.is_error() && !addr_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "int_to_ptr".to_string(),
                    expected: "u64".to_string(),
                    found: self.format_type_name(addr_type),
                })),
                span,
            ));
        }

        // Get the result type from HM inference (must be a ptr mut T)
        let result_type = Self::get_resolved_type(ctx, inst_ref, span, "@int_to_ptr intrinsic")?;

        // Validate that the inferred type is a mutable pointer
        if !result_type.is_ptr_mut() && !result_type.is_error() && !result_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "int_to_ptr".to_string(),
                    expected: "MutPtr(T)".to_string(),
                    found: self.format_type_name(result_type),
                })),
                span,
            ));
        }

        // Create the intrinsic call instruction
        let args_start = air.add_extra(&[addr_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 1,
            },
            ty: result_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_type))
    }

    /// Analyze @null_ptr intrinsic: creates a typed null pointer.
    /// Signature: @null_ptr() -> ptr const T
    /// The result type T is inferred from context (e.g., `let p: ptr const i32 = @null_ptr()`)
    pub(super) fn analyze_null_ptr_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "null_ptr".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        // Get the result type from HM inference (must be a pointer type)
        let result_type = Self::get_resolved_type(ctx, inst_ref, span, "@null_ptr intrinsic")?;

        // Validate that the inferred type is a pointer
        if !result_type.is_ptr() && !result_type.is_error() && !result_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "null_ptr".to_string(),
                    expected: "Ptr(T) or MutPtr(T)".to_string(),
                    found: self.format_type_name(result_type),
                })),
                span,
            ));
        }

        // Create the intrinsic call instruction (no args)
        let args_start = air.add_extra(&[]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 0,
            },
            ty: result_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_type))
    }

    /// Analyze @is_null intrinsic: checks if a pointer is null.
    /// Signature: @is_null(ptr: ptr T) -> bool
    pub(super) fn analyze_is_null_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "is_null".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        let ptr_result = self.analyze_inst(air, args[0].value, ctx)?;
        let ptr_type = ptr_result.ty;

        // Validate pointer type
        if !ptr_type.is_ptr() && !ptr_type.is_error() && !ptr_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "is_null".to_string(),
                    expected: "Ptr(T) or MutPtr(T)".to_string(),
                    found: self.format_type_name(ptr_type),
                })),
                span,
            ));
        }

        // Create the intrinsic call instruction (returns bool)
        let args_start = air.add_extra(&[ptr_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 1,
            },
            ty: Type::BOOL,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::BOOL))
    }

    /// Analyze @ptr_copy intrinsic: copies n elements from src to dst.
    /// Signature: @ptr_copy(dst: ptr mut T, src: ptr const T, count: usize) -> ()
    pub(super) fn analyze_ptr_copy_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 3 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "ptr_copy".to_string(),
                    expected: 3,
                    found: args.len(),
                },
                span,
            ));
        }

        let dst_result = self.analyze_inst(air, args[0].value, ctx)?;
        let src_result = self.analyze_inst(air, args[1].value, ctx)?;
        let count_result = self.analyze_inst(air, args[2].value, ctx)?;
        let dst_type = dst_result.ty;
        let src_type = src_result.ty;
        let count_type = count_result.ty;

        // dst must be ptr mut T
        let dst_pointee = match dst_type.kind() {
            TypeKind::PtrMut(ptr_id) => self.type_pool.ptr_mut_def(ptr_id),
            TypeKind::PtrConst(_) => {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: "ptr_copy".to_string(),
                        expected: "MutPtr(T) (cannot copy into Ptr)".to_string(),
                        found: self.format_type_name(dst_type),
                    })),
                    span,
                ));
            }
            _ => {
                if !dst_type.is_error() && !dst_type.is_never() {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                            name: "ptr_copy".to_string(),
                            expected: "MutPtr(T)".to_string(),
                            found: self.format_type_name(dst_type),
                        })),
                        span,
                    ));
                }
                Type::ERROR
            }
        };

        // src must be ptr const T or ptr mut T
        let src_pointee = match src_type.kind() {
            TypeKind::PtrConst(ptr_id) => self.type_pool.ptr_const_def(ptr_id),
            TypeKind::PtrMut(ptr_id) => self.type_pool.ptr_mut_def(ptr_id),
            _ => {
                if !src_type.is_error() && !src_type.is_never() {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                            name: "ptr_copy".to_string(),
                            expected: "Ptr(T) or MutPtr(T)".to_string(),
                            found: self.format_type_name(src_type),
                        })),
                        span,
                    ));
                }
                Type::ERROR
            }
        };

        // Pointee types must match
        if dst_pointee != src_pointee
            && !dst_pointee.is_error()
            && !src_pointee.is_error()
            && !dst_pointee.is_never()
            && !src_pointee.is_never()
        {
            return Err(CompileError::type_mismatch(
                self.format_type_name(dst_pointee),
                self.format_type_name(src_pointee),
                span,
            ));
        }

        // count must be `usize` (ADR-0054).
        if count_type != Type::USIZE && !count_type.is_error() && !count_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "ptr_copy".to_string(),
                    expected: "usize".to_string(),
                    found: self.format_type_name(count_type),
                })),
                span,
            ));
        }

        // Create the intrinsic call instruction
        let args_start = air.add_extra(&[
            dst_result.air_ref.as_u32(),
            src_result.air_ref.as_u32(),
            count_result.air_ref.as_u32(),
        ]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 3,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
    }

    /// Analyze @addr_of / @addr_of_mut intrinsics: takes address of lvalue.
    /// Signature: @addr_of(lvalue) -> ptr const T
    /// Signature: @addr_of_mut(lvalue) -> ptr mut T
    pub(super) fn analyze_addr_of_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
        is_mut: bool,
    ) -> CompileResult<AnalysisResult> {
        let intrinsic_name = if is_mut { "addr_of_mut" } else { "addr_of" };

        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: intrinsic_name.to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        let arg_result = self.analyze_inst(air, args[0].value, ctx)?;
        let pointee_type = arg_result.ty;

        // For addr_of, we need the argument to be an lvalue (addressable)
        // This is validated at the RIR level - here we just compute the result type

        // Create the pointer type
        let result_type = if is_mut {
            let ptr_type_id = self.type_pool.intern_ptr_mut_from_type(pointee_type);
            Type::new_ptr_mut(ptr_type_id)
        } else {
            let ptr_type_id = self.type_pool.intern_ptr_const_from_type(pointee_type);
            Type::new_ptr_const(ptr_type_id)
        };

        // Create the intrinsic call instruction
        let name = if is_mut {
            self.known.raw_mut
        } else {
            self.known.raw
        };
        let args_start = air.add_extra(&[arg_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 1,
            },
            ty: result_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_type))
    }

    /// ADR-0063: dispatch an associated-function call `Ptr(T)::name(args)` /
    /// `MutPtr(T)::name(args)` through the [`POINTER_METHODS`] registry.
    ///
    /// `lhs_type_sym` is the synthesized type-call symbol (e.g., `Ptr(i32)`)
    /// that astgen produced for the AssocFnCall LHS. We resolve it through
    /// `resolve_type` to recover the concrete pointer type, then look the
    /// function up in the registry and lower as the equivalent intrinsic.
    pub(crate) fn dispatch_pointer_assoc_fn_call(
        &mut self,
        air: &mut Air,
        lhs_type_sym: Spur,
        function_name: &str,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let lhs_type = self.resolve_type(lhs_type_sym, span)?;
        let ptr_kind = match lhs_type.kind() {
            TypeKind::PtrConst(_) => PointerKind::Ptr,
            TypeKind::PtrMut(_) => PointerKind::MutPtr,
            _ => {
                // Resolved to a non-pointer type — fall through to the
                // standard error path by reporting an unknown type.
                return Err(CompileError::new(
                    ErrorKind::UnknownType(self.format_type_name(lhs_type)),
                    span,
                ));
            }
        };

        let entry = lookup_pointer_method(ptr_kind, function_name, PointerOpForm::AssocFn)
            .ok_or_else(|| {
                CompileError::new(
                    ErrorKind::UndefinedMethod {
                        type_name: self.format_type_name(lhs_type),
                        method_name: function_name.to_string(),
                    },
                    span,
                )
            })?;

        if entry.requires_checked {
            Self::require_checked_for_intrinsic(ctx, entry.intrinsic_name, span)?;
        }

        let intrinsic_name_sym = self.interner.get_or_intern(entry.intrinsic_name);
        self.lower_pointer_op_to_air(
            air,
            PointerOpKind {
                intrinsic: entry.intrinsic,
                name: intrinsic_name_sym,
                op_name: function_name,
            },
            PointerOpOrigin {
                receiver: None,
                lhs_type: Some(lhs_type),
            },
            args,
            span,
            ctx,
        )
    }

    /// ADR-0071: dispatch an associated-function call `char::name(args)`.
    ///
    /// v1:
    /// - `char::from_u32_unchecked(n: u32) -> char` — `checked` block only;
    ///   lowers to `IntCast` from u32 to char (no-op at LLVM level since both
    ///   are `i32`).
    /// - `char::from_u32(n: u32) -> Result(char, u32)` — calls into the
    ///   prelude function `char__from_u32`, which performs the range check
    ///   and constructs the Result.
    ///
    /// ADR-0088 unified the unchecked-fn gate around the `is_unchecked`
    /// flag on the resolved fn/method; the dispatch on `char::from_u32_unchecked`
    /// participates in the same gating discipline (the caller must wrap
    /// the call in `checked { }`). `char` doesn't have a struct
    /// `MethodInfo` slot because it's a primitive, so the check is
    /// inline here rather than going through `MethodInfo::is_unchecked`.
    pub(crate) fn dispatch_char_assoc_fn_call(
        &mut self,
        air: &mut Air,
        function_name: &str,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        match function_name {
            "from_u32_unchecked" => {
                // ADR-0088: the same `UncheckedCallRequiresChecked` path
                // that fires for `@mark(unchecked)` methods. `char` is
                // a primitive (no struct method table), so the gate
                // is applied inline.
                if ctx.checked_depth == 0 {
                    return Err(CompileError::new(
                        ErrorKind::UncheckedCallRequiresChecked(
                            "char::from_u32_unchecked".to_string(),
                        ),
                        span,
                    ));
                }
                if args.len() != 1 {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 1,
                            found: args.len(),
                        },
                        span,
                    ));
                }
                let arg_result = self.analyze_inst(air, args[0].value, ctx)?;
                if arg_result.ty != Type::U32 {
                    return Err(CompileError::type_mismatch(
                        "u32".to_string(),
                        arg_result.ty.name().to_string(),
                        span,
                    ));
                }
                // u32 and char share the i32 LLVM lowering; emit IntCast
                // for type bookkeeping (codegen is a no-op).
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::IntCast {
                        value: arg_result.air_ref,
                        from_ty: Type::U32,
                    },
                    ty: Type::CHAR,
                    span,
                });
                Ok(AnalysisResult::new(air_ref, Type::CHAR))
            }
            "from_u32" => {
                // Delegate to the prelude function `char__from_u32` which
                // performs the range check and constructs `Result(char, u32)`.
                if args.len() != 1 {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 1,
                            found: args.len(),
                        },
                        span,
                    ));
                }
                let arg_result = self.analyze_inst(air, args[0].value, ctx)?;
                if arg_result.ty != Type::U32 {
                    return Err(CompileError::type_mismatch(
                        "u32".to_string(),
                        arg_result.ty.name().to_string(),
                        span,
                    ));
                }
                let fn_name = self.interner.get_or_intern("char__from_u32");
                let fn_info = self.functions.get(&fn_name).ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::InternalError(
                            "prelude function `char__from_u32` not found".to_string(),
                        ),
                        span,
                    )
                })?;
                let return_ty = fn_info.return_type;
                ctx.referenced_functions.insert(fn_name);
                let extra = vec![arg_result.air_ref.as_u32(), AirArgMode::Normal.as_u32()];
                let args_start = air.add_extra(&extra);
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Call {
                        name: fn_name,
                        args_start,
                        args_len: 1,
                    },
                    ty: return_ty,
                    span,
                });
                Ok(AnalysisResult::new(air_ref, return_ty))
            }
            _ => Err(CompileError::new(
                ErrorKind::UndefinedFunction(format!("char::{}", function_name)),
                span,
            )),
        }
    }

    /// ADR-0071: dispatch a method call `c.name(args)` on a `char` value.
    ///
    /// v1 surface (Phases 3 + 5):
    /// - `c.to_u32() -> u32` — no-op cast (char and u32 share i32 lowering).
    /// - `c.len_utf8() -> usize` — branchless arithmetic on the codepoint.
    /// - `c.is_ascii() -> bool` — `c.to_u32() < 128`.
    /// - `c.encode_utf8(buf: &mut [u8; 4]) -> usize` — Phase 5 inline lowering.
    pub(crate) fn dispatch_char_method_call(
        &mut self,
        air: &mut Air,
        receiver_result: AnalysisResult,
        method_name: &str,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        match method_name {
            "to_u32" => {
                if !args.is_empty() {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicWrongArgCount {
                            name: "char.to_u32".to_string(),
                            expected: 0,
                            found: args.len(),
                        },
                        span,
                    ));
                }
                // char and u32 share the i32 LLVM lowering; emit an IntCast
                // for type-tag bookkeeping (codegen treats char→u32 as a no-op).
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::IntCast {
                        value: receiver_result.air_ref,
                        from_ty: Type::CHAR,
                    },
                    ty: Type::U32,
                    span,
                });
                Ok(AnalysisResult::new(air_ref, Type::U32))
            }
            "is_ascii" | "len_utf8" | "encode_utf8" => {
                let expected_args = if method_name == "encode_utf8" { 1 } else { 0 };
                if args.len() != expected_args {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicWrongArgCount {
                            name: format!("char.{}", method_name),
                            expected: expected_args,
                            found: args.len(),
                        },
                        span,
                    ));
                }
                // Dispatch to the prelude helper function with the receiver
                // as the first argument, plus any user args.
                let fn_name_str = format!("char__{}", method_name);
                let fn_name = self.interner.get_or_intern(&fn_name_str);
                let fn_info = self.functions.get(&fn_name).ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::InternalError(format!(
                            "prelude function `{}` not found",
                            fn_name_str
                        )),
                        span,
                    )
                })?;
                let return_ty = fn_info.return_type;
                let param_modes: Vec<_> = self.param_arena.modes(fn_info.params).to_vec();
                ctx.referenced_functions.insert(fn_name);

                let _ = param_modes;
                let mut extra = vec![
                    receiver_result.air_ref.as_u32(),
                    AirArgMode::Normal.as_u32(),
                ];
                // Use the standard call-args helper so `&x` / `&mut x`
                // arguments un-move their underlying variable after
                // analysis (ADR-0080 made arrays non-Copy, surfacing the
                // gap that the previous direct `analyze_inst` loop left).
                let air_args = self.analyze_call_args(air, args, ctx)?;
                for air_arg in air_args {
                    extra.push(air_arg.value.as_u32());
                    extra.push(air_arg.mode.as_u32());
                }
                let args_len = (args.len() + 1) as u32;
                let args_start = air.add_extra(&extra);
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Call {
                        name: fn_name,
                        args_start,
                        args_len,
                    },
                    ty: return_ty,
                    span,
                });
                Ok(AnalysisResult::new(air_ref, return_ty))
            }
            _ => Err(CompileError::new(
                ErrorKind::UndefinedMethod {
                    type_name: "char".to_string(),
                    method_name: method_name.to_string(),
                },
                span,
            )),
        }
    }

    /// ADR-0063: dispatch a method call `p.name(args)` on a `Ptr(T)` /
    /// `MutPtr(T)` value through the [`POINTER_METHODS`] registry.
    ///
    /// The receiver has already been analysed by the caller; this function
    /// only handles the explicit `args` (which still need analysing) and emits
    /// the equivalent intrinsic AIR instruction. It does not re-analyse the
    /// receiver to avoid emitting duplicate AIR for it.
    pub(crate) fn dispatch_pointer_method_call(
        &mut self,
        air: &mut Air,
        receiver_result: AnalysisResult,
        method_name: &str,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let ptr_kind = match receiver_result.ty.kind() {
            TypeKind::PtrConst(_) => PointerKind::Ptr,
            TypeKind::PtrMut(_) => PointerKind::MutPtr,
            _ => unreachable!("dispatch_pointer_method_call called with non-pointer receiver"),
        };

        let entry = lookup_pointer_method(ptr_kind, method_name, PointerOpForm::Method)
            .ok_or_else(|| {
                CompileError::new(
                    ErrorKind::UndefinedMethod {
                        type_name: self.format_type_name(receiver_result.ty),
                        method_name: method_name.to_string(),
                    },
                    span,
                )
            })?;

        if entry.requires_checked {
            Self::require_checked_for_intrinsic(ctx, entry.intrinsic_name, span)?;
        }

        let intrinsic_name_sym = self.interner.get_or_intern(entry.intrinsic_name);
        self.lower_pointer_op_to_air(
            air,
            PointerOpKind {
                intrinsic: entry.intrinsic,
                name: intrinsic_name_sym,
                op_name: method_name,
            },
            PointerOpOrigin {
                receiver: Some(receiver_result),
                lhs_type: None,
            },
            args,
            span,
            ctx,
        )
    }

    /// ADR-0064: dispatch a method call `s.name(args)` on a `Slice(T)` /
    /// `MutSlice(T)` value through the SLICE_METHODS registry.
    pub(crate) fn dispatch_slice_method_call(
        &mut self,
        air: &mut Air,
        receiver_result: AnalysisResult,
        method_name: &str,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        use gruel_intrinsics::{SliceKind, lookup_slice_method};

        let slice_kind = match receiver_result.ty.kind() {
            TypeKind::Slice(_) => SliceKind::Slice,
            TypeKind::MutSlice(_) => SliceKind::MutSlice,
            _ => unreachable!("dispatch_slice_method_call called with non-slice receiver"),
        };

        let entry = lookup_slice_method(slice_kind, method_name).ok_or_else(|| {
            CompileError::new(
                ErrorKind::UndefinedMethod {
                    type_name: self.format_type_name(receiver_result.ty),
                    method_name: method_name.to_string(),
                },
                span,
            )
        })?;

        if entry.requires_checked {
            Self::require_checked_for_intrinsic(ctx, entry.intrinsic_name, span)?;
        }

        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        let result_ty = match entry.intrinsic {
            IntrinsicId::SliceLen => Type::USIZE,
            IntrinsicId::SliceIsEmpty => Type::BOOL,
            IntrinsicId::SlicePtr => {
                let elem_ty = match receiver_result.ty.kind() {
                    TypeKind::Slice(id) => self.type_pool.slice_def(id),
                    TypeKind::MutSlice(id) => self.type_pool.mut_slice_def(id),
                    _ => unreachable!("slice receiver"),
                };
                let id = self.type_pool.intern_ptr_const_from_type(elem_ty);
                Type::new_ptr_const(id)
            }
            IntrinsicId::SlicePtrMut => {
                let elem_ty = match receiver_result.ty.kind() {
                    TypeKind::MutSlice(id) => self.type_pool.mut_slice_def(id),
                    _ => unreachable!("ptr_mut requires MutSlice receiver"),
                };
                let id = self.type_pool.intern_ptr_mut_from_type(elem_ty);
                Type::new_ptr_mut(id)
            }
            _ => unreachable!("SLICE_METHODS only references slice intrinsics"),
        };

        let intrinsic_name_sym = self.interner.get_or_intern(entry.intrinsic_name);
        let args_start = air.add_extra(&[receiver_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: intrinsic_name_sym,
                args_start,
                args_len: 1,
            },
            ty: result_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_ty))
    }

    /// ADR-0064: `@parts_to_slice(p, n)` / `@parts_to_mut_slice(p, n)`.
    pub(crate) fn analyze_parts_to_slice_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
        is_mut: bool,
    ) -> CompileResult<AnalysisResult> {
        let intrinsic_name = if is_mut {
            "parts_to_mut_slice"
        } else {
            "parts_to_slice"
        };
        if args.len() != 2 {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: 2,
                    found: args.len(),
                },
                span,
            ));
        }
        let p = self.analyze_inst(air, args[0].value, ctx)?;
        let n = self.analyze_inst(air, args[1].value, ctx)?;

        let elem_ty = match (p.ty.kind(), is_mut) {
            (TypeKind::PtrConst(id), false) => self.type_pool.ptr_const_def(id),
            (TypeKind::PtrMut(id), true) => self.type_pool.ptr_mut_def(id),
            _ => {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: intrinsic_name.to_string(),
                        expected: if is_mut { "MutPtr(T)" } else { "Ptr(T)" }.to_string(),
                        found: self.format_type_name(p.ty),
                    })),
                    span,
                ));
            }
        };
        if n.ty != Type::USIZE && !n.ty.is_error() && !n.ty.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: intrinsic_name.to_string(),
                    expected: "usize".to_string(),
                    found: self.format_type_name(n.ty),
                })),
                span,
            ));
        }

        let result_ty = if is_mut {
            let id = self.type_pool.intern_mut_slice_from_type(elem_ty);
            Type::new_mut_slice(id)
        } else {
            let id = self.type_pool.intern_slice_from_type(elem_ty);
            Type::new_slice(id)
        };

        let name_sym = self.interner.get_or_intern(intrinsic_name);
        let args_start = air.add_extra(&[p.air_ref.as_u32(), n.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: name_sym,
                args_start,
                args_len: 2,
            },
            ty: result_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_ty))
    }

    /// Shared lowering for ADR-0063 pointer methods and associated functions.
    ///
    /// Each entry maps to an existing [`IntrinsicId`] and reuses the same
    /// type-checking shape that the corresponding `analyze_*_intrinsic`
    /// function uses. The receiver, when present, must already be analysed —
    /// for method calls the caller is `dispatch_pointer_method_call` and for
    /// associated-function calls it is the path-call dispatch.
    fn lower_pointer_op_to_air(
        &mut self,
        air: &mut Air,
        op: PointerOpKind<'_>,
        origin: PointerOpOrigin,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let PointerOpKind {
            intrinsic,
            name: intrinsic_name,
            op_name,
        } = op;
        let PointerOpOrigin { receiver, lhs_type } = origin;
        // ---- Helpers ----
        let make_intrinsic = |air: &mut Air, arg_refs: &[u32], ty: Type| -> AirRef {
            let args_start = air.add_extra(arg_refs);
            air.add_inst(AirInst {
                data: AirInstData::Intrinsic {
                    name: intrinsic_name,
                    args_start,
                    args_len: arg_refs.len() as u32,
                },
                ty,
                span,
            })
        };

        match intrinsic {
            // p.read() / p.read_volatile() — receiver: pointer; args: 0; result: pointee T
            IntrinsicId::PtrRead | IntrinsicId::PtrReadVolatile => {
                if !args.is_empty() {
                    return Err(self.pointer_op_arg_count_err(op_name, 0, args.len(), span));
                }
                let recv = receiver.expect("read is a method");
                let pointee = self.pointer_pointee_type(recv.ty);
                let air_ref = make_intrinsic(air, &[recv.air_ref.as_u32()], pointee);
                Ok(AnalysisResult::new(air_ref, pointee))
            }
            // p.write(v) / p.write_volatile(v) — receiver: MutPtr(T); args: [v: T]; result: ()
            IntrinsicId::PtrWrite | IntrinsicId::PtrWriteVolatile => {
                if args.len() != 1 {
                    return Err(self.pointer_op_arg_count_err(op_name, 1, args.len(), span));
                }
                let recv = receiver.expect("write is a method");
                let pointee = match recv.ty.kind() {
                    TypeKind::PtrMut(id) => self.type_pool.ptr_mut_def(id),
                    _ => unreachable!("write only available on MutPtr"),
                };
                let v = self.analyze_inst(air, args[0].value, ctx)?;
                if v.ty != pointee && !v.ty.is_error() && !v.ty.is_never() {
                    return Err(CompileError::type_mismatch(
                        self.format_type_name(pointee),
                        self.format_type_name(v.ty),
                        span,
                    ));
                }
                let air_ref = make_intrinsic(
                    air,
                    &[recv.air_ref.as_u32(), v.air_ref.as_u32()],
                    Type::UNIT,
                );
                Ok(AnalysisResult::new(air_ref, Type::UNIT))
            }
            // p.offset(n) — receiver: pointer; args: [n: integer]; result: Self
            IntrinsicId::PtrOffset => {
                if args.len() != 1 {
                    return Err(self.pointer_op_arg_count_err(op_name, 1, args.len(), span));
                }
                let recv = receiver.expect("offset is a method");
                let n = self.analyze_inst(air, args[0].value, ctx)?;
                if !n.ty.is_integer() && !n.ty.is_error() && !n.ty.is_never() {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                            name: op_name.to_string(),
                            expected: "integer offset".to_string(),
                            found: self.format_type_name(n.ty),
                        })),
                        span,
                    ));
                }
                let air_ref =
                    make_intrinsic(air, &[recv.air_ref.as_u32(), n.air_ref.as_u32()], recv.ty);
                Ok(AnalysisResult::new(air_ref, recv.ty))
            }
            // p.is_null() — receiver: pointer; args: 0; result: bool
            IntrinsicId::IsNull => {
                if !args.is_empty() {
                    return Err(self.pointer_op_arg_count_err(op_name, 0, args.len(), span));
                }
                let recv = receiver.expect("is_null is a method");
                let air_ref = make_intrinsic(air, &[recv.air_ref.as_u32()], Type::BOOL);
                Ok(AnalysisResult::new(air_ref, Type::BOOL))
            }
            // p.to_int() — receiver: pointer; args: 0; result: u64
            IntrinsicId::PtrToInt => {
                if !args.is_empty() {
                    return Err(self.pointer_op_arg_count_err(op_name, 0, args.len(), span));
                }
                let recv = receiver.expect("to_int is a method");
                let air_ref = make_intrinsic(air, &[recv.air_ref.as_u32()], Type::U64);
                Ok(AnalysisResult::new(air_ref, Type::U64))
            }
            // dst.copy_from(src, n) — receiver: MutPtr(T); args: [src: pointer-of-T, n: usize];
            // result: ()
            IntrinsicId::PtrCopy => {
                if args.len() != 2 {
                    return Err(self.pointer_op_arg_count_err(op_name, 2, args.len(), span));
                }
                let recv = receiver.expect("copy_from is a method");
                let dst_pointee = self.pointer_pointee_type(recv.ty);
                let src = self.analyze_inst(air, args[0].value, ctx)?;
                let n = self.analyze_inst(air, args[1].value, ctx)?;
                let src_pointee = self.pointer_pointee_type(src.ty);
                if dst_pointee != src_pointee && !dst_pointee.is_error() && !src_pointee.is_error()
                {
                    return Err(CompileError::type_mismatch(
                        self.format_type_name(dst_pointee),
                        self.format_type_name(src_pointee),
                        span,
                    ));
                }
                if n.ty != Type::USIZE && !n.ty.is_error() && !n.ty.is_never() {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                            name: op_name.to_string(),
                            expected: "usize".to_string(),
                            found: self.format_type_name(n.ty),
                        })),
                        span,
                    ));
                }
                // ptr_copy intrinsic order: (dst, src, n).
                let air_ref = make_intrinsic(
                    air,
                    &[
                        recv.air_ref.as_u32(),
                        src.air_ref.as_u32(),
                        n.air_ref.as_u32(),
                    ],
                    Type::UNIT,
                );
                Ok(AnalysisResult::new(air_ref, Type::UNIT))
            }
            // Ptr(T)::from(r) — args: [r: Ref(T)]; result: Ptr(T)
            // MutPtr(T)::from(r) — args: [r: MutRef(T)]; result: MutPtr(T)
            IntrinsicId::Raw | IntrinsicId::RawMut => {
                if args.len() != 1 {
                    return Err(self.pointer_op_arg_count_err(op_name, 1, args.len(), span));
                }
                let target_kind = if intrinsic == IntrinsicId::RawMut {
                    PointerKind::MutPtr
                } else {
                    PointerKind::Ptr
                };
                let result_type = lhs_type.ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::InternalError(
                            "Ptr/MutPtr::from without LHS type context".to_string(),
                        ),
                        span,
                    )
                })?;
                let pointee_from_lhs = self.pointer_pointee_type(result_type);
                let r = self.analyze_inst(air, args[0].value, ctx)?;
                let (ref_pointee, ref_kind) = match r.ty.kind() {
                    TypeKind::Ref(id) => (self.type_pool.ref_def(id), PointerKind::Ptr),
                    TypeKind::MutRef(id) => (self.type_pool.mut_ref_def(id), PointerKind::MutPtr),
                    _ => {
                        return Err(CompileError::new(
                            ErrorKind::IntrinsicTypeMismatch(Box::new(
                                IntrinsicTypeMismatchError {
                                    name: format!("{}::from", pointer_kind_name(target_kind)),
                                    expected: format!(
                                        "{}({})",
                                        if target_kind == PointerKind::Ptr {
                                            "Ref"
                                        } else {
                                            "MutRef"
                                        },
                                        self.format_type_name(pointee_from_lhs)
                                    ),
                                    found: self.format_type_name(r.ty),
                                },
                            )),
                            span,
                        ));
                    }
                };
                if ref_kind != target_kind {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                            name: format!("{}::from", pointer_kind_name(target_kind)),
                            expected: format!(
                                "{}({})",
                                if target_kind == PointerKind::Ptr {
                                    "Ref"
                                } else {
                                    "MutRef"
                                },
                                self.format_type_name(pointee_from_lhs)
                            ),
                            found: self.format_type_name(r.ty),
                        })),
                        span,
                    ));
                }
                if ref_pointee != pointee_from_lhs
                    && !ref_pointee.is_error()
                    && !pointee_from_lhs.is_error()
                {
                    return Err(CompileError::type_mismatch(
                        self.format_type_name(pointee_from_lhs),
                        self.format_type_name(ref_pointee),
                        span,
                    ));
                }
                let air_ref = make_intrinsic(air, &[r.air_ref.as_u32()], result_type);
                Ok(AnalysisResult::new(air_ref, result_type))
            }
            // Ptr(T)::null() / MutPtr(T)::null() — args: 0; result: Self (from LHS)
            IntrinsicId::NullPtr => {
                if !args.is_empty() {
                    return Err(self.pointer_op_arg_count_err(op_name, 0, args.len(), span));
                }
                let result_type = lhs_type.ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::InternalError("null without LHS type context".to_string()),
                        span,
                    )
                })?;
                let air_ref = make_intrinsic(air, &[], result_type);
                Ok(AnalysisResult::new(air_ref, result_type))
            }
            // Ptr(T)::from_int(addr) / MutPtr(T)::from_int(addr) — args: [addr: u64]; result: Self
            IntrinsicId::IntToPtr => {
                if args.len() != 1 {
                    return Err(self.pointer_op_arg_count_err(op_name, 1, args.len(), span));
                }
                let result_type = lhs_type.ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::InternalError("from_int without LHS type context".to_string()),
                        span,
                    )
                })?;
                let addr = self.analyze_inst(air, args[0].value, ctx)?;
                if addr.ty != Type::U64 && !addr.ty.is_error() && !addr.ty.is_never() {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                            name: op_name.to_string(),
                            expected: "u64".to_string(),
                            found: self.format_type_name(addr.ty),
                        })),
                        span,
                    ));
                }
                let air_ref = make_intrinsic(air, &[addr.air_ref.as_u32()], result_type);
                Ok(AnalysisResult::new(air_ref, result_type))
            }
            other => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "POINTER_METHODS entry references unhandled intrinsic {:?}",
                    other
                )),
                span,
            )),
        }
    }

    fn pointer_op_arg_count_err(
        &self,
        name: &str,
        expected: usize,
        found: usize,
        span: Span,
    ) -> CompileError {
        CompileError::new(ErrorKind::WrongArgumentCount { expected, found }, span)
            .with_note(format!("`{}` expects {} argument(s)", name, expected))
    }

    fn pointer_pointee_type(&self, ty: Type) -> Type {
        match ty.kind() {
            TypeKind::PtrConst(id) => self.type_pool.ptr_const_def(id),
            TypeKind::PtrMut(id) => self.type_pool.ptr_mut_def(id),
            _ => Type::ERROR,
        }
    }
}

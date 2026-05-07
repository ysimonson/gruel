//! Vec(T) method dispatch (ADR-0066).
//!
//! Vec methods are emitted as `Intrinsic` AIR nodes by sema, with codegen
//! lowering each intrinsic to inline LLVM. Static-method calls
//! (`Vec(T)::new()`, `Vec(T)::with_capacity(n)`) and instance method calls
//! (`v.push(x)`, `v.len()`, etc.) both flow through this module.

use gruel_rir::{InstRef, RirCallArg};
use gruel_util::{CompileError, CompileResult, ErrorKind, Span};

use super::Sema;
use super::context::{AnalysisContext, AnalysisResult};
use crate::AirArgMode;
use crate::types::{Type, TypeKind};
use crate::{Air, AirInst, AirInstData};

impl<'a> Sema<'a> {
    /// Dispatch a static method call (`V::new()` / `V::with_capacity(n)`)
    /// on a comptime-resolved Vec type. Returns `Some` if `V` is a Vec, else
    /// `None` (caller falls through to other dispatch paths).
    pub(crate) fn try_dispatch_vec_static_call(
        &mut self,
        air: &mut Air,
        ctx: &mut AnalysisContext,
        vec_ty: Type,
        function_name: &str,
        args: &[RirCallArg],
        span: Span,
    ) -> Option<CompileResult<AnalysisResult>> {
        let TypeKind::Vec(vec_id) = vec_ty.kind() else {
            return None;
        };
        match function_name {
            "new" => {
                if !args.is_empty() {
                    return Some(Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 0,
                            found: args.len(),
                        },
                        span,
                    )));
                }
                let _ = vec_id;
                Some(self.emit_vec_intrinsic(air, "vec_new", &[], vec_ty, span))
            }
            "with_capacity" => {
                if args.len() != 1 {
                    return Some(Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 1,
                            found: args.len(),
                        },
                        span,
                    )));
                }
                let n_res = match self.analyze_inst(air, args[0].value, ctx) {
                    Ok(r) => r,
                    Err(e) => return Some(Err(e)),
                };
                if !n_res.ty.is_integer() && !n_res.ty.is_error() {
                    return Some(Err(CompileError::type_mismatch(
                        "usize".to_string(),
                        self.format_type_name(n_res.ty),
                        span,
                    )));
                }
                Some(self.emit_vec_intrinsic(
                    air,
                    "vec_with_capacity",
                    &[(n_res.air_ref, AirArgMode::Normal)],
                    vec_ty,
                    span,
                ))
            }
            _ => Some(Err(CompileError::new(
                ErrorKind::UndefinedAssocFn {
                    type_name: self.format_type_name(vec_ty),
                    function_name: function_name.to_string(),
                },
                span,
            ))),
        }
    }

    /// Dispatch an instance method call on a Vec receiver
    /// (`v.push(x)`, `v.len()`, etc.).
    pub(crate) fn dispatch_vec_method_call(
        &mut self,
        air: &mut Air,
        receiver: AnalysisResult,
        method_name: &str,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let vec_id = match receiver.ty.kind() {
            TypeKind::Vec(id) => id,
            _ => unreachable!("dispatch_vec_method_call called with non-Vec receiver"),
        };
        let elem_ty = self.type_pool.vec_def(vec_id);

        match method_name {
            "len" => self.emit_vec_query(air, "vec_len", receiver, args, span, Type::USIZE),
            "capacity" => {
                self.emit_vec_query(air, "vec_capacity", receiver, args, span, Type::USIZE)
            }
            "is_empty" => {
                self.emit_vec_query(air, "vec_is_empty", receiver, args, span, Type::BOOL)
            }
            "push" => {
                if args.len() != 1 {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 1,
                            found: args.len(),
                        },
                        span,
                    ));
                }
                let val = self.analyze_inst(air, args[0].value, ctx)?;
                if val.ty != elem_ty && !val.ty.is_error() {
                    return Err(CompileError::type_mismatch(
                        self.format_type_name(elem_ty),
                        self.format_type_name(val.ty),
                        span,
                    ));
                }
                self.emit_vec_intrinsic(
                    air,
                    "vec_push",
                    &[
                        (receiver.air_ref, AirArgMode::MutRef),
                        (val.air_ref, AirArgMode::Normal),
                    ],
                    Type::UNIT,
                    span,
                )
            }
            "pop" => {
                if !args.is_empty() {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 0,
                            found: args.len(),
                        },
                        span,
                    ));
                }
                // Returns `Option(T)` — but Option requires the prelude /
                // generic-enum machinery; for v1 codegen path returns the
                // bare element type and the surface-level Option wrapping is
                // a future-work refinement. Returning T preserves
                // bounds-checked move-out semantics for the success case.
                self.emit_vec_intrinsic(
                    air,
                    "vec_pop",
                    &[(receiver.air_ref, AirArgMode::MutRef)],
                    elem_ty,
                    span,
                )
            }
            "clear" => {
                if !args.is_empty() {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 0,
                            found: args.len(),
                        },
                        span,
                    ));
                }
                self.emit_vec_intrinsic(
                    air,
                    "vec_clear",
                    &[(receiver.air_ref, AirArgMode::MutRef)],
                    Type::UNIT,
                    span,
                )
            }
            "reserve" => {
                if args.len() != 1 {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 1,
                            found: args.len(),
                        },
                        span,
                    ));
                }
                let n = self.analyze_inst(air, args[0].value, ctx)?;
                if !n.ty.is_integer() && !n.ty.is_error() {
                    return Err(CompileError::type_mismatch(
                        "usize".to_string(),
                        self.format_type_name(n.ty),
                        span,
                    ));
                }
                self.emit_vec_intrinsic(
                    air,
                    "vec_reserve",
                    &[
                        (receiver.air_ref, AirArgMode::MutRef),
                        (n.air_ref, AirArgMode::Normal),
                    ],
                    Type::UNIT,
                    span,
                )
            }
            "ptr" => {
                Self::require_checked_for_intrinsic(ctx, "vec_ptr", span)?;
                let id = self.type_pool.intern_ptr_const_from_type(elem_ty);
                self.emit_vec_intrinsic(
                    air,
                    "vec_ptr",
                    &[(receiver.air_ref, AirArgMode::Ref)],
                    Type::new_ptr_const(id),
                    span,
                )
            }
            "ptr_mut" => {
                Self::require_checked_for_intrinsic(ctx, "vec_ptr_mut", span)?;
                let id = self.type_pool.intern_ptr_mut_from_type(elem_ty);
                self.emit_vec_intrinsic(
                    air,
                    "vec_ptr_mut",
                    &[(receiver.air_ref, AirArgMode::MutRef)],
                    Type::new_ptr_mut(id),
                    span,
                )
            }
            "terminated_ptr" => {
                Self::require_checked_for_intrinsic(ctx, "vec_terminated_ptr", span)?;
                if args.len() != 1 {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 1,
                            found: args.len(),
                        },
                        span,
                    ));
                }
                let s = self.analyze_inst(air, args[0].value, ctx)?;
                if s.ty != elem_ty && !s.ty.is_error() {
                    return Err(CompileError::type_mismatch(
                        self.format_type_name(elem_ty),
                        self.format_type_name(s.ty),
                        span,
                    ));
                }
                let id = self.type_pool.intern_ptr_const_from_type(elem_ty);
                self.emit_vec_intrinsic(
                    air,
                    "vec_terminated_ptr",
                    &[
                        (receiver.air_ref, AirArgMode::MutRef),
                        (s.air_ref, AirArgMode::Normal),
                    ],
                    Type::new_ptr_const(id),
                    span,
                )
            }
            "clone" => {
                // ADR-0066 Phase 11: per-element clone for non-Copy `T`
                // requires emitting per-element clone calls (e.g.
                // `String__clone`) which depends on field-of-borrow access
                // that the language doesn't yet support cleanly. Until that
                // lands, `Vec(T).clone()` is restricted to `T: Copy` and the
                // shallow-memcpy path. Reject non-Copy elements with a clear
                // error rather than silently aliasing the heap buffer.
                if !self.is_type_copy(elem_ty) {
                    return Err(CompileError::new(
                        ErrorKind::InternalError(format!(
                            "Vec(T).clone() requires T: Copy in v1 (T = {}); \
                             per-element clone is deferred — see ADR-0066 Phase 11",
                            self.format_type_name(elem_ty)
                        )),
                        span,
                    ));
                }
                self.emit_vec_query(air, "vec_clone", receiver, args, span, receiver.ty)
            }
            "dispose" => {
                if !args.is_empty() {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 0,
                            found: args.len(),
                        },
                        span,
                    ));
                }
                // dispose consumes self by-value (Normal arg mode).
                self.emit_vec_intrinsic(
                    air,
                    "vec_dispose",
                    &[(receiver.air_ref, AirArgMode::Normal)],
                    Type::UNIT,
                    span,
                )
            }
            // ADR-0081 Phase 1: byte-comparison and search methods.
            "eq" | "cmp" => {
                self.dispatch_vec_eq_cmp(air, receiver, elem_ty, method_name, args, span, ctx)
            }
            "contains" | "starts_with" | "ends_with" => self.dispatch_vec_byte_search(
                air,
                receiver,
                elem_ty,
                method_name,
                args,
                span,
                ctx,
            ),
            "concat" => self.dispatch_vec_concat(air, receiver, elem_ty, args, span, ctx),
            "extend_from_slice" => {
                self.dispatch_vec_extend_from_slice(air, receiver, elem_ty, args, span, ctx)
            }
            _ => Err(CompileError::new(
                ErrorKind::UndefinedMethod {
                    type_name: self.format_type_name(receiver.ty),
                    method_name: method_name.to_string(),
                },
                span,
            )),
        }
    }

    /// Emit a no-arg query method like `len`/`capacity`/`is_empty`/`clone`.
    fn emit_vec_query(
        &mut self,
        air: &mut Air,
        intrinsic_name: &str,
        receiver: AnalysisResult,
        args: &[RirCallArg],
        span: Span,
        ret_ty: Type,
    ) -> CompileResult<AnalysisResult> {
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }
        self.emit_vec_intrinsic(
            air,
            intrinsic_name,
            &[(receiver.air_ref, AirArgMode::Ref)],
            ret_ty,
            span,
        )
    }

    fn emit_vec_intrinsic(
        &mut self,
        air: &mut Air,
        intrinsic_name: &str,
        args: &[(crate::AirRef, AirArgMode)],
        ret_ty: Type,
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // Intrinsic AIR encodes args as flat air_refs (no modes); the modes
        // are tracked by codegen based on which intrinsic is being lowered.
        let extra: Vec<u32> = args.iter().map(|(r, _)| r.as_u32()).collect();
        let args_start = air.add_extra(&extra);
        let name_sym = self.interner.get_or_intern(intrinsic_name);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: name_sym,
                args_start,
                args_len: args.len() as u32,
            },
            ty: ret_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, ret_ty))
    }

    /// ADR-0081 Phase 1: dispatch `v.eq(other)` / `v.cmp(other)` on
    /// `Vec(T)` where `T: Copy`. Both `self` and `other` are passed by
    /// `Ref` (no move) — comparison reads values without consuming them.
    fn dispatch_vec_eq_cmp(
        &mut self,
        air: &mut Air,
        receiver: AnalysisResult,
        elem_ty: Type,
        method_name: &str,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if !self.is_type_copy(elem_ty) {
            return Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "Vec(T).{}() requires T: Copy in v1 (T = {}); \
                     non-Copy element comparison is deferred — see ADR-0081",
                    method_name,
                    self.format_type_name(elem_ty)
                )),
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
        let other = self.analyze_inst_for_projection(air, args[0].value, ctx)?;
        if other.ty != receiver.ty && !other.ty.is_error() && !receiver.ty.is_error() {
            return Err(CompileError::type_mismatch(
                self.format_type_name(receiver.ty),
                self.format_type_name(other.ty),
                span,
            ));
        }
        let (intrinsic_name, ret_ty) = if method_name == "eq" {
            ("vec_eq", Type::BOOL)
        } else {
            // `cmp` returns `Ordering`. Resolve the cached enum id; fall
            // back to a clear error if the prelude isn't loaded.
            let ordering_id = self
                .lang_items
                .ordering()
                .or(self.builtin_ordering_id)
                .ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::InternalError(
                            "Ordering enum not found (prelude not loaded?)".into(),
                        ),
                        span,
                    )
                })?;
            ("vec_cmp", Type::new_enum(ordering_id))
        };
        self.emit_vec_intrinsic(
            air,
            intrinsic_name,
            &[
                (receiver.air_ref, AirArgMode::Ref),
                (other.air_ref, AirArgMode::Ref),
            ],
            ret_ty,
            span,
        )
    }

    /// ADR-0081 Phase 1: dispatch `v.contains(needle)`,
    /// `v.starts_with(prefix)`, `v.ends_with(suffix)` on `Vec(T)` where
    /// `T: Copy`. The argument is `Slice(T)`; receiver is `Ref(Self)`.
    fn dispatch_vec_byte_search(
        &mut self,
        air: &mut Air,
        receiver: AnalysisResult,
        elem_ty: Type,
        method_name: &str,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if !self.is_type_copy(elem_ty) {
            return Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "Vec(T).{}() requires T: Copy in v1 (T = {}); \
                     non-Copy element search is deferred — see ADR-0081",
                    method_name,
                    self.format_type_name(elem_ty)
                )),
                span,
            ));
        }
        let intrinsic_name = match method_name {
            "contains" => "vec_contains",
            "starts_with" => "vec_starts_with",
            "ends_with" => "vec_ends_with",
            _ => unreachable!(),
        };
        let other = self.analyze_slice_arg_for_vec(air, intrinsic_name, elem_ty, args, span, ctx)?;
        self.emit_vec_intrinsic(
            air,
            intrinsic_name,
            &[
                (receiver.air_ref, AirArgMode::Ref),
                (other.air_ref, AirArgMode::Normal),
            ],
            Type::BOOL,
            span,
        )
    }

    /// ADR-0081 Phase 1: dispatch `v.concat(other)` on `Vec(T)` where
    /// `T: Copy`. Receiver is `Ref(Self)`; argument is `Slice(T)`;
    /// returns a freshly-allocated `Vec(T)`.
    fn dispatch_vec_concat(
        &mut self,
        air: &mut Air,
        receiver: AnalysisResult,
        elem_ty: Type,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if !self.is_type_copy(elem_ty) {
            return Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "Vec(T).concat() requires T: Copy in v1 (T = {}); \
                     non-Copy concat is deferred — see ADR-0081",
                    self.format_type_name(elem_ty)
                )),
                span,
            ));
        }
        let other = self.analyze_slice_arg_for_vec(air, "vec_concat", elem_ty, args, span, ctx)?;
        self.emit_vec_intrinsic(
            air,
            "vec_concat",
            &[
                (receiver.air_ref, AirArgMode::Ref),
                (other.air_ref, AirArgMode::Normal),
            ],
            receiver.ty,
            span,
        )
    }

    /// ADR-0081 Phase 1: dispatch `v.extend_from_slice(other)` on `Vec(T)`
    /// where `T: Copy`. Receiver is `MutRef(Self)`; argument is `Slice(T)`.
    fn dispatch_vec_extend_from_slice(
        &mut self,
        air: &mut Air,
        receiver: AnalysisResult,
        elem_ty: Type,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if !self.is_type_copy(elem_ty) {
            return Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "Vec(T).extend_from_slice() requires T: Copy in v1 (T = {}); \
                     non-Copy extend is deferred — see ADR-0081",
                    self.format_type_name(elem_ty)
                )),
                span,
            ));
        }
        let other =
            self.analyze_slice_arg_for_vec(air, "vec_extend_from_slice", elem_ty, args, span, ctx)?;
        self.emit_vec_intrinsic(
            air,
            "vec_extend_from_slice",
            &[
                (receiver.air_ref, AirArgMode::MutRef),
                (other.air_ref, AirArgMode::Normal),
            ],
            Type::UNIT,
            span,
        )
    }

    /// Shared helper: analyze the single `Slice(T)` argument for the byte
    /// search / concat / extend_from_slice methods.
    fn analyze_slice_arg_for_vec(
        &mut self,
        air: &mut Air,
        _intrinsic_name: &str,
        elem_ty: Type,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }
        // The argument is conceptually a borrow (Slice(T) sits over the
        // source storage). Use the projection path so the source isn't
        // recorded as moved into the call.
        let other = self.analyze_inst_for_projection(air, args[0].value, ctx)?;
        let expected_slice_id = self.type_pool.intern_slice_from_type(elem_ty);
        let expected_ty = Type::new_slice(expected_slice_id);
        // Accept either `Slice(T)` or `MutSlice(T)` — both are valid input
        // for read-only sequence ops.
        let acceptable = match other.ty.kind() {
            TypeKind::Slice(id) => self.type_pool.slice_def(id) == elem_ty,
            TypeKind::MutSlice(id) => self.type_pool.mut_slice_def(id) == elem_ty,
            _ => false,
        };
        if !acceptable && !other.ty.is_error() {
            return Err(CompileError::type_mismatch(
                self.format_type_name(expected_ty),
                self.format_type_name(other.ty),
                span,
            ));
        }
        Ok(other)
    }

    /// `@vec(a, b, c)`: variadic literal construction.
    pub(crate) fn analyze_vec_literal_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: 1,
                    found: 0,
                },
                span,
            ));
        }
        let mut analyzed = Vec::with_capacity(args.len());
        let mut elem_ty: Option<Type> = None;
        for arg in args {
            let res = self.analyze_inst(air, arg.value, ctx)?;
            match elem_ty {
                None => elem_ty = Some(res.ty),
                Some(t) => {
                    if t != res.ty && !res.ty.is_error() && !t.is_error() {
                        return Err(CompileError::type_mismatch(
                            self.format_type_name(t),
                            self.format_type_name(res.ty),
                            span,
                        ));
                    }
                }
            }
            analyzed.push(res.air_ref);
        }
        let elem_ty = elem_ty.unwrap();
        if self.is_type_linear(elem_ty) {
            return Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "@vec does not support linear element types (T = {})",
                    self.format_type_name(elem_ty)
                )),
                span,
            ));
        }
        let vec_id = self.type_pool.intern_vec_from_type(elem_ty);
        let vec_ty = Type::new_vec(vec_id);

        let extra: Vec<u32> = analyzed.iter().map(|r| r.as_u32()).collect();
        let args_start = air.add_extra(&extra);
        let name_sym = self.interner.get_or_intern("vec");
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: name_sym,
                args_start,
                args_len: analyzed.len() as u32,
            },
            ty: vec_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, vec_ty))
    }

    /// `@vec_repeat(v, n)`: build a Vec from N copies of a single value.
    pub(crate) fn analyze_vec_repeat_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 2 {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: 2,
                    found: args.len(),
                },
                span,
            ));
        }
        let v = self.analyze_inst(air, args[0].value, ctx)?;
        let n = self.analyze_inst(air, args[1].value, ctx)?;
        if !n.ty.is_integer() && !n.ty.is_error() {
            return Err(CompileError::type_mismatch(
                "usize".to_string(),
                self.format_type_name(n.ty),
                span,
            ));
        }
        if self.is_type_linear(v.ty) {
            return Err(CompileError::new(
                ErrorKind::InternalError(
                    "@vec_repeat does not support linear element types".to_string(),
                ),
                span,
            ));
        }
        // v1: T must be Copy (no recursive clone synthesis).
        if !self.is_type_copy(v.ty) {
            return Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "@vec_repeat requires T: Copy in v1 (T = {}); see ADR-0066",
                    self.format_type_name(v.ty)
                )),
                span,
            ));
        }
        let vec_id = self.type_pool.intern_vec_from_type(v.ty);
        let vec_ty = Type::new_vec(vec_id);

        let extra = vec![v.air_ref.as_u32(), n.air_ref.as_u32()];
        let args_start = air.add_extra(&extra);
        let name_sym = self.interner.get_or_intern("vec_repeat");
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: name_sym,
                args_start,
                args_len: 2,
            },
            ty: vec_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, vec_ty))
    }

    /// Try to analyze `v[i]` where `v` is a Vec(T). Returns `Ok(None)` to
    /// fall through to array indexing if the base isn't a Vec.
    pub(crate) fn try_analyze_vec_index_read(
        &mut self,
        air: &mut Air,
        base: InstRef,
        index: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Option<AnalysisResult>> {
        let peek_ty = self.peek_inst_type(base, ctx);
        if !matches!(peek_ty.map(|t: Type| t.kind()), Some(TypeKind::Vec(_))) {
            return Ok(None);
        }
        let base_var = self.extract_root_variable(base);
        let base_res = self.analyze_inst(air, base, ctx)?;
        // Indexing borrows the receiver — undo any move that analyze_inst
        // recorded for the root variable.
        if let Some(var) = base_var {
            ctx.moved_vars.remove(&var);
        }
        let elem_ty = match base_res.ty.kind() {
            TypeKind::Vec(id) => self.type_pool.vec_def(id),
            _ => return Ok(None),
        };
        if !self.is_type_copy(elem_ty) {
            return Err(CompileError::new(
                ErrorKind::MoveOutOfIndex {
                    element_type: self.format_type_name(elem_ty),
                },
                span,
            ));
        }
        let index_res = self.analyze_inst(air, index, ctx)?;
        if !index_res.ty.is_integer() && !index_res.ty.is_error() && !index_res.ty.is_never() {
            return Err(CompileError::type_mismatch(
                "usize".to_string(),
                self.format_type_name(index_res.ty),
                self.rir.get(index).span,
            ));
        }
        let extra = vec![base_res.air_ref.as_u32(), index_res.air_ref.as_u32()];
        let args_start = air.add_extra(&extra);
        let name = self.interner.get_or_intern("vec_index_read");
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 2,
            },
            ty: elem_ty,
            span,
        });
        Ok(Some(AnalysisResult::new(air_ref, elem_ty)))
    }

    /// Try to analyze `v[i] = x` where `v` is a Vec(T).
    pub(crate) fn try_analyze_vec_index_write(
        &mut self,
        air: &mut Air,
        base: InstRef,
        index: InstRef,
        value: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Option<AnalysisResult>> {
        let peek_ty = self.peek_inst_type(base, ctx);
        if !matches!(peek_ty.map(|t: Type| t.kind()), Some(TypeKind::Vec(_))) {
            return Ok(None);
        }
        let base_var = self.extract_root_variable(base);
        let base_res = self.analyze_inst(air, base, ctx)?;
        if let Some(var) = base_var {
            ctx.moved_vars.remove(&var);
        }
        let elem_ty = match base_res.ty.kind() {
            TypeKind::Vec(id) => self.type_pool.vec_def(id),
            _ => return Ok(None),
        };
        let index_res = self.analyze_inst(air, index, ctx)?;
        let value_res = self.analyze_inst(air, value, ctx)?;
        if value_res.ty != elem_ty && !value_res.ty.is_error() {
            return Err(CompileError::type_mismatch(
                self.format_type_name(elem_ty),
                self.format_type_name(value_res.ty),
                span,
            ));
        }
        let extra = vec![
            base_res.air_ref.as_u32(),
            index_res.air_ref.as_u32(),
            value_res.air_ref.as_u32(),
        ];
        let args_start = air.add_extra(&extra);
        let name = self.interner.get_or_intern("vec_index_write");
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 3,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(Some(AnalysisResult::new(air_ref, Type::UNIT)))
    }

    /// `@parts_to_vec(p, len, cap)`: build a Vec from raw parts.
    pub(crate) fn analyze_parts_to_vec_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        Self::require_checked_for_intrinsic(ctx, "parts_to_vec", span)?;
        if args.len() != 3 {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: 3,
                    found: args.len(),
                },
                span,
            ));
        }
        let p = self.analyze_inst(air, args[0].value, ctx)?;
        let len = self.analyze_inst(air, args[1].value, ctx)?;
        let cap = self.analyze_inst(air, args[2].value, ctx)?;
        let elem_ty = match p.ty.kind() {
            TypeKind::PtrMut(id) => self.type_pool.ptr_mut_def(id),
            _ => {
                return Err(CompileError::type_mismatch(
                    "MutPtr(T)".to_string(),
                    self.format_type_name(p.ty),
                    span,
                ));
            }
        };
        for arg_res in &[&len, &cap] {
            if !arg_res.ty.is_integer() && !arg_res.ty.is_error() {
                return Err(CompileError::type_mismatch(
                    "usize".to_string(),
                    self.format_type_name(arg_res.ty),
                    span,
                ));
            }
        }
        let vec_id = self.type_pool.intern_vec_from_type(elem_ty);
        let vec_ty = Type::new_vec(vec_id);

        let extra = vec![
            p.air_ref.as_u32(),
            len.air_ref.as_u32(),
            cap.air_ref.as_u32(),
        ];
        let args_start = air.add_extra(&extra);
        let name_sym = self.interner.get_or_intern("parts_to_vec");
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: name_sym,
                args_start,
                args_len: 3,
            },
            ty: vec_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, vec_ty))
    }
}

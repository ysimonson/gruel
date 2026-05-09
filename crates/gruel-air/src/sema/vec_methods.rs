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
        let elem_ty = self.type_pool.vec_def(vec_id);

        // ADR-0082: route every `Vec(T)::fn(...)` static call through the
        // prelude struct's associated function. The prelude registry
        // is populated lazily by `try_dispatch_vec_static_via_prelude`,
        // so by the time this returns Ok(None) we know `function_name`
        // isn't a method on the prelude struct — surface that as a
        // proper `UndefinedAssocFn` error.
        match self.try_dispatch_vec_static_via_prelude(
            air,
            vec_ty,
            elem_ty,
            function_name,
            args,
            span,
            ctx,
        ) {
            Ok(Some(result)) => Some(Ok(result)),
            Ok(None) => Some(Err(CompileError::new(
                ErrorKind::UndefinedAssocFn {
                    type_name: self.format_type_name(vec_ty),
                    function_name: function_name.to_string(),
                },
                span,
            ))),
            Err(e) => Some(Err(e)),
        }
    }

    /// ADR-0082: route a `Vec(T)::name(args)` static call through the
    /// prelude struct's associated function (`has_self == false`).
    /// Returns `Ok(None)` if the prelude struct or function isn't yet
    /// registered, or `Err(_)` for a found-but-mis-arity'd call.
    fn try_dispatch_vec_static_via_prelude(
        &mut self,
        air: &mut Air,
        vec_ty: Type,
        elem_ty: Type,
        function_name: &str,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Option<AnalysisResult>> {
        self.populate_vec_instance(elem_ty);
        let Some(struct_id) = self.vec_instance_for_elem(elem_ty) else {
            return Ok(None);
        };
        let fn_sym = self.interner.get_or_intern(function_name);
        let Some(method_info) = self.methods.get(&(struct_id, fn_sym)).copied() else {
            return Ok(None);
        };
        if method_info.has_self {
            return Ok(None);
        }

        let method_param_types = self.param_arena.types(method_info.params).to_vec();
        if args.len() != method_param_types.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: method_param_types.len(),
                    found: args.len(),
                },
                span,
            ));
        }

        let mut air_args = self.analyze_call_args(air, args, ctx)?;
        // ADR-0082: HM doesn't see anonymous-struct method signatures
        // when a parameterized type call (`Vec(I32)::with_capacity(8)`)
        // is constraint-generated, so integer literals among the args
        // default to `i32`. The dispatch site has the method's resolved
        // param types — bridge the gap by emitting `IntCast` for any
        // integer arg whose type doesn't match.
        for (slot, arg) in air_args.iter_mut().enumerate() {
            let expected = method_param_types[slot];
            let actual = air.get(arg.value).ty;
            if actual != expected
                && actual.is_integer()
                && expected.is_integer()
                && !actual.is_error()
                && !expected.is_error()
            {
                let cast_ref = air.add_inst(AirInst {
                    data: AirInstData::IntCast {
                        value: arg.value,
                        from_ty: actual,
                    },
                    ty: expected,
                    span,
                });
                arg.value = cast_ref;
            }
        }
        let struct_def = self.type_pool.struct_def(struct_id);
        // Static (associated) methods are mangled with `::`; instance
        // methods use `.`. Mirrors how the work queue assigns the
        // analyzed function's `full_name` so the AIR Call resolves
        // against the same symbol the codegen emits.
        let call_name = format!("{}::{}", struct_def.name, function_name);
        let call_name_sym = self.interner.get_or_intern(&call_name);
        let args_len = air_args.len() as u32;
        let mut extra_data = Vec::with_capacity(air_args.len() * 2);
        for arg in &air_args {
            extra_data.push(arg.value.as_u32());
            extra_data.push(arg.mode.as_u32());
        }
        let args_start = air.add_extra(&extra_data);
        // The prelude method returns `Self` (the prelude struct). Cast
        // to the legacy `TypeKind::Vec(_)` for compatibility with the
        // rest of sema until full elimination in Phase 5. Layouts
        // match, so the AIR-level type pun is safe at codegen.
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: call_name_sym,
                args_start,
                args_len,
            },
            ty: vec_ty,
            span,
        });
        Ok(Some(AnalysisResult::new(air_ref, vec_ty)))
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

        // ADR-0082: methods whose v1 contract requires `T: Copy`
        // (clone, eq, cmp, contains, starts_with, ends_with, concat,
        // extend_from_slice). The prelude bodies use `ptr.read()` /
        // `slice[i]` which sema would reject for non-Copy `T` with a
        // generic "cannot move out of indexed position" — but the spec
        // demands the more specific "Vec(T).{method}() requires T: Copy"
        // diagnostic. Gate at dispatch so the user-visible error is the
        // one the spec calls for. Future work (per-element clone
        // synthesis) will lift these gates.
        if matches!(
            method_name,
            "clone"
                | "eq"
                | "cmp"
                | "contains"
                | "starts_with"
                | "ends_with"
                | "concat"
                | "extend_from_slice"
        ) && !self.is_type_copy(elem_ty)
        {
            let reason = if method_name == "clone" {
                format!(
                    "Vec(T).clone() requires T: Copy in v1 (T = {}); \
                     per-element clone is deferred — see ADR-0066 Phase 11",
                    self.format_type_name(elem_ty)
                )
            } else {
                format!(
                    "Vec(T).{}() requires T: Copy in v1 (T = {}); \
                     non-Copy element comparison/search/concat is deferred — see ADR-0081",
                    method_name,
                    self.format_type_name(elem_ty)
                )
            };
            return Err(CompileError::new(ErrorKind::InternalError(reason), span));
        }

        // ADR-0082 Phase 4: route through the prelude
        // `@lang("vec")` declaration's instantiated methods. The
        // receiver value (TypeKind::Vec(_)) and the prelude struct's
        // `Self` (TypeKind::Struct(StructId)) share an identical
        // {ptr, len, cap} layout, so the AIR-level type pun is safe
        // and codegen sees the same LLVM aggregate. Falls through to
        // the legacy codegen-inline path only for methods the prelude
        // doesn't define (kept for byte-search methods etc. until
        // those land in the prelude as well).
        // ADR-0082: route every Vec instance method through the prelude
        // struct's instantiated method. The prelude registry is
        // populated lazily by `try_dispatch_vec_method_via_prelude`.
        // `Ok(None)` means the method isn't on the prelude struct —
        // surface it as a proper `UndefinedMethod` diagnostic.
        if let Some(result) = self.try_dispatch_vec_method_via_prelude(
            air,
            receiver,
            elem_ty,
            method_name,
            args,
            span,
            ctx,
        )? {
            return Ok(result);
        }
        Err(CompileError::new(
            ErrorKind::UndefinedMethod {
                type_name: self.format_type_name(receiver.ty),
                method_name: method_name.to_string(),
            },
            span,
        ))
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
        // ADR-0082: route `v[i]` through the prelude struct's
        // `index_read(i)` method. With the prelude struct registered
        // for every Vec(T) elem, this always succeeds — the
        // `try_emit_prelude_index_call -> Ok(None)` path is unreachable.
        let result = self
            .try_emit_prelude_index_call(
                air,
                base_res,
                elem_ty,
                "index_read",
                &[index_res],
                elem_ty,
                span,
            )?
            .expect("Vec index_read: prelude method must be registered");
        Ok(Some(result))
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
        // ADR-0082: route `v[i] = x` through the prelude struct's
        // `index_write(i, x)` method. The prelude struct always has
        // the method registered for any Vec(T).
        let result = self
            .try_emit_prelude_index_call(
                air,
                base_res,
                elem_ty,
                "index_write",
                &[index_res, value_res],
                Type::UNIT,
                span,
            )?
            .expect("Vec index_write: prelude method must be registered");
        Ok(Some(result))
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

    /// ADR-0082: route a Vec indexing operation (`v[i]` /
    /// `v[i] = x`) through the prelude struct's `index_read` /
    /// `index_write` method. Receiver mode is determined by the
    /// method's signature. Returns `Ok(None)` if the prelude struct
    /// or method isn't yet registered (caller falls back to legacy).
    fn try_emit_prelude_index_call(
        &mut self,
        air: &mut Air,
        receiver: AnalysisResult,
        elem_ty: Type,
        method_name: &str,
        analyzed_args: &[AnalysisResult],
        return_type: Type,
        span: Span,
    ) -> CompileResult<Option<AnalysisResult>> {
        self.populate_vec_instance(elem_ty);
        let Some(struct_id) = self.vec_instance_for_elem(elem_ty) else {
            return Ok(None);
        };
        let method_sym = self.interner.get_or_intern(method_name);
        let Some(method_info) = self.methods.get(&(struct_id, method_sym)).copied() else {
            return Ok(None);
        };
        if !method_info.has_self {
            return Ok(None);
        }
        let recv_pass_mode = match method_info.receiver {
            crate::types::ReceiverMode::ByValue => AirArgMode::Normal,
            crate::types::ReceiverMode::Ref => AirArgMode::Ref,
            crate::types::ReceiverMode::MutRef => AirArgMode::MutRef,
        };

        let mut air_args = vec![crate::AirCallArg {
            value: receiver.air_ref,
            mode: recv_pass_mode,
        }];
        for r in analyzed_args {
            air_args.push(crate::AirCallArg {
                value: r.air_ref,
                mode: AirArgMode::Normal,
            });
        }

        // ADR-0082: int-literal coercion for index_read/index_write —
        // `v[0]` etc. lowers to `index_read(0: int_literal)` and the
        // method param is `usize`.
        let m_param_types = self.param_arena.types(method_info.params).to_vec();
        for (slot, arg) in air_args.iter_mut().skip(1).enumerate() {
            let expected = m_param_types[slot];
            let actual = air.get(arg.value).ty;
            if actual != expected
                && actual.is_integer()
                && expected.is_integer()
                && !actual.is_error()
                && !expected.is_error()
            {
                let cast_ref = air.add_inst(AirInst {
                    data: AirInstData::IntCast {
                        value: arg.value,
                        from_ty: actual,
                    },
                    ty: expected,
                    span,
                });
                arg.value = cast_ref;
            }
        }

        let struct_def = self.type_pool.struct_def(struct_id);
        let call_name = format!("{}.{}", struct_def.name, method_name);
        let call_name_sym = self.interner.get_or_intern(&call_name);
        let args_len = air_args.len() as u32;
        let mut extra_data = Vec::with_capacity(air_args.len() * 2);
        for arg in &air_args {
            extra_data.push(arg.value.as_u32());
            extra_data.push(arg.mode.as_u32());
        }
        let args_start = air.add_extra(&extra_data);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: call_name_sym,
                args_start,
                args_len,
            },
            ty: return_type,
            span,
        });
        Ok(Some(AnalysisResult::new(air_ref, return_type)))
    }

    /// ADR-0082: route a Vec method call through the prelude
    /// `@lang("vec")` declaration's instantiated struct methods.
    /// Returns `Ok(Some(result))` on success, `Ok(None)` if the
    /// prelude declaration / registry doesn't yet have an entry for
    /// this element type or the method (caller falls back to legacy
    /// codegen-inline path), or `Err(_)` if the prelude method exists
    /// but its arity / arg types don't match.
    fn try_dispatch_vec_method_via_prelude(
        &mut self,
        air: &mut Air,
        receiver: AnalysisResult,
        elem_ty: Type,
        method_name: &str,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Option<AnalysisResult>> {
        // Ensure the prelude struct exists for this element type.
        self.populate_vec_instance(elem_ty);
        let Some(struct_id) = self.vec_instance_for_elem(elem_ty) else {
            return Ok(None);
        };
        let method_sym = self.interner.get_or_intern(method_name);
        let Some(method_info) = self.methods.get(&(struct_id, method_sym)).copied() else {
            return Ok(None);
        };
        if !method_info.has_self {
            return Ok(None);
        }
        let recv_pass_mode = match method_info.receiver {
            crate::types::ReceiverMode::ByValue => AirArgMode::Normal,
            crate::types::ReceiverMode::Ref => AirArgMode::Ref,
            crate::types::ReceiverMode::MutRef => AirArgMode::MutRef,
        };

        // Track lazy analysis (the method body may not have been
        // analyzed yet — this matches the work-queue logic in
        // analyze_method_call_impl).
        ctx.referenced_methods.insert((struct_id, method_sym));

        let method_param_types = self.param_arena.types(method_info.params).to_vec();
        if args.len() != method_param_types.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: method_param_types.len(),
                    found: args.len(),
                },
                span,
            ));
        }

        let mut air_args = vec![crate::AirCallArg {
            value: receiver.air_ref,
            mode: recv_pass_mode,
        }];
        air_args.extend(self.analyze_call_args(air, args, ctx)?);

        // ADR-0082: bridge HM's int-literal default → method's
        // resolved param type, and align arg-mode with param-mode.
        // Without the mode alignment, a `Ref(Self)` param receives the
        // arg by value (struct aggregate) while codegen expects a
        // pointer — the LLVM verifier rejects it.
        let m_param_types = self.param_arena.types(method_info.params).to_vec();
        for (slot, arg) in air_args.iter_mut().skip(1).enumerate() {
            let expected = m_param_types[slot];
            let actual = air.get(arg.value).ty;
            // Mode alignment: if the param is `Ref(T)` / `MutRef(T)`,
            // promote the arg's mode so codegen passes by pointer.
            match expected.kind() {
                crate::types::TypeKind::Ref(_) => arg.mode = AirArgMode::Ref,
                crate::types::TypeKind::MutRef(_) => arg.mode = AirArgMode::MutRef,
                _ => {}
            }
            if actual != expected
                && actual.is_integer()
                && expected.is_integer()
                && !actual.is_error()
                && !expected.is_error()
            {
                let cast_ref = air.add_inst(AirInst {
                    data: AirInstData::IntCast {
                        value: arg.value,
                        from_ty: actual,
                    },
                    ty: expected,
                    span,
                });
                arg.value = cast_ref;
            }
        }

        let struct_def = self.type_pool.struct_def(struct_id);
        let call_name = format!("{}.{}", struct_def.name, method_name);
        let call_name_sym = self.interner.get_or_intern(&call_name);

        // ADR-0082: when the method returns `Self` (the prelude
        // struct), the user-facing surface expects `Vec(T)` — same
        // layout, different `TypeKind`. Pun the return type so the
        // surrounding sema (assignments, struct field constraints,
        // etc.) sees the legacy `TypeKind::Vec(_)`. The codegen-level
        // aggregate is identical, so the pun is safe.
        let prelude_struct_ty = Type::new_struct(struct_id);
        let return_type = if method_info.return_type == prelude_struct_ty {
            receiver.ty
        } else {
            method_info.return_type
        };
        let args_len = air_args.len() as u32;
        let mut extra_data = Vec::with_capacity(air_args.len() * 2);
        for arg in &air_args {
            extra_data.push(arg.value.as_u32());
            extra_data.push(arg.mode.as_u32());
        }
        let args_start = air.add_extra(&extra_data);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: call_name_sym,
                args_start,
                args_len,
            },
            ty: return_type,
            span,
        });
        Ok(Some(AnalysisResult::new(air_ref, return_type)))
    }
}

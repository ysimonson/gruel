//! Intrinsic call analysis for the semantic-analysis pass.
//!
//! Hosts `analyze_intrinsic_impl` (the dispatcher driven by the
//! `gruel-intrinsics` registry) plus every per-intrinsic helper that
//! does not belong to a more cohesive subsystem (pointer ops live in
//! `pointer_ops.rs`; vector ops live in `vec_methods.rs`). Also hosts
//! `resolve_import_path` / `resolve_std_import` because they are
//! exclusively driven by `@import` analysis.

use gruel_intrinsics::{IntrinsicId, lookup_by_id};
use gruel_rir::{InstRef, RirCallArg};
use gruel_util::{
    CompileError, CompileResult, ErrorKind, IntrinsicTypeMismatchError, PreviewFeature, Span,
};
use lasso::Spur;

use super::Sema;
use super::analysis::{arch_variant_index, os_variant_index};
use super::context::{AnalysisContext, AnalysisResult, ComptimeHeapItem, ConstValue};
use crate::inst::{Air, AirInst, AirInstData};
use crate::types::{Type, TypeKind};

impl<'a> Sema<'a> {
    /// Implementation for Intrinsic calls.
    ///
    /// Dispatches on the stable `IntrinsicId` resolved from the
    /// `gruel-intrinsics` registry. The per-intrinsic analyzer functions still
    /// live in this file; only the dispatcher changed with ADR-0050.
    pub(crate) fn analyze_intrinsic_impl(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        name: Spur,
        args: Vec<RirCallArg>,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let Some(id) = self.known.intrinsic_id(name) else {
            let intrinsic_name_str = self.interner.resolve(&name);
            return Err(CompileError::new(
                ErrorKind::UnknownIntrinsic(intrinsic_name_str.to_string()),
                span,
            ));
        };
        let def = lookup_by_id(id);

        // Registry-driven gates: preview feature and unchecked-block requirement.
        if let Some(feature) = def.preview {
            let what = format!("@{}() intrinsic", def.name);
            self.require_preview(feature, &what, span)?;
        }
        if def.requires_unchecked {
            Self::require_checked_for_intrinsic(ctx, def.name, span)?;
        }

        // ADR-0063: pointer intrinsics are no longer reachable through the
        // `@…` namespace. Their `IntrinsicId` variants stay (so codegen can
        // dispatch from the new `p.method(...)` / `Ptr(T)::name(...)`
        // surface form) but using them via `@name(...)` is rejected with a
        // pointer to the new spelling.
        if matches!(
            id,
            IntrinsicId::PtrRead
                | IntrinsicId::PtrWrite
                | IntrinsicId::PtrReadVolatile
                | IntrinsicId::PtrWriteVolatile
                | IntrinsicId::PtrOffset
                | IntrinsicId::PtrToInt
                | IntrinsicId::IntToPtr
                | IntrinsicId::NullPtr
                | IntrinsicId::IsNull
                | IntrinsicId::PtrCopy
                | IntrinsicId::Raw
                | IntrinsicId::RawMut
        ) {
            return Err(CompileError::new(
                ErrorKind::UnknownIntrinsic(format!(
                    "{} (replaced by Ptr(T)/MutPtr(T) methods, ADR-0063)",
                    def.name
                )),
                span,
            ));
        }

        match id {
            IntrinsicId::Dbg => self.analyze_dbg_intrinsic(air, inst_ref, &args, span, ctx),
            IntrinsicId::TestPreviewGate => {
                self.analyze_test_preview_gate_intrinsic(air, &args, span)
            }
            IntrinsicId::ReadLine => self.analyze_read_line_intrinsic(air, name, &args, span),
            IntrinsicId::ParseI32
            | IntrinsicId::ParseI64
            | IntrinsicId::ParseU32
            | IntrinsicId::ParseU64 => {
                self.analyze_parse_intrinsic(air, name, def.name, &args, span, ctx)
            }
            IntrinsicId::Cast => self.analyze_cast_intrinsic(air, inst_ref, &args, span, ctx),
            IntrinsicId::Panic => self.analyze_panic_intrinsic(air, &args, span, ctx),
            IntrinsicId::Assert => self.analyze_assert_intrinsic(air, &args, span, ctx),
            IntrinsicId::Import => self.analyze_import_intrinsic(air, &args, span),
            IntrinsicId::EmbedFile => self.analyze_embed_file_intrinsic(air, &args, span, ctx),
            IntrinsicId::RandomU32 => self.analyze_random_u32_intrinsic(air, name, &args, span),
            IntrinsicId::RandomU64 => self.analyze_random_u64_intrinsic(air, name, &args, span),
            IntrinsicId::PtrRead | IntrinsicId::PtrReadVolatile => {
                self.analyze_ptr_read_intrinsic(air, name, &args, span, ctx)
            }
            IntrinsicId::PtrWrite | IntrinsicId::PtrWriteVolatile => {
                self.analyze_ptr_write_intrinsic(air, name, &args, span, ctx)
            }
            IntrinsicId::PtrOffset => {
                self.analyze_ptr_offset_intrinsic(air, name, &args, span, ctx)
            }
            IntrinsicId::PtrToInt => self.analyze_ptr_to_int_intrinsic(air, name, &args, span, ctx),
            IntrinsicId::IntToPtr => {
                self.analyze_int_to_ptr_intrinsic(air, name, inst_ref, &args, span, ctx)
            }
            IntrinsicId::NullPtr => {
                self.analyze_null_ptr_intrinsic(air, name, inst_ref, &args, span, ctx)
            }
            IntrinsicId::IsNull => self.analyze_is_null_intrinsic(air, name, &args, span, ctx),
            IntrinsicId::PtrCopy => self.analyze_ptr_copy_intrinsic(air, name, &args, span, ctx),
            IntrinsicId::Raw => self.analyze_addr_of_intrinsic(air, &args, span, ctx, false),
            IntrinsicId::RawMut => self.analyze_addr_of_intrinsic(air, &args, span, ctx, true),
            IntrinsicId::Syscall => self.analyze_syscall_intrinsic(air, name, &args, span, ctx),
            IntrinsicId::TargetArch => self.analyze_target_arch_intrinsic(air, &args, span),
            IntrinsicId::TargetOs => self.analyze_target_os_intrinsic(air, &args, span),
            IntrinsicId::CompileError => self.analyze_compile_error_intrinsic(air, &args, span),
            IntrinsicId::Field => self.analyze_field_intrinsic(air, &args, span, ctx),
            // Type intrinsics are handled via the `TypeIntrinsic` RIR node, not
            // this path. @range is consumed as an iterable by analyze_ops. If
            // any of these ids do reach the expression dispatcher it's a usage
            // error, matching the pre-registry fall-through behavior.
            IntrinsicId::SizeOf
            | IntrinsicId::AlignOf
            | IntrinsicId::TypeName
            | IntrinsicId::TypeInfo
            | IntrinsicId::Ownership
            | IntrinsicId::Implements
            | IntrinsicId::Range
            // Slice methods/indexing are dispatched via the SLICE_METHODS
            // registry and `analyze_index_*`, not as direct expression-position
            // intrinsics. Reaching this arm means the user wrote
            // `@slice_len(s)` etc. directly — treat as unknown for now (the
            // surface form is `s.len()` / `s[i]`).
            | IntrinsicId::SliceLen
            | IntrinsicId::SliceIsEmpty
            | IntrinsicId::SliceIndexRead
            | IntrinsicId::SliceIndexWrite
            | IntrinsicId::SlicePtr
            | IntrinsicId::SlicePtrMut
            // ADR-0066: Vec methods are dispatched via VEC_METHODS / static
            // path-call paths, not direct @vec_* expressions.
            | IntrinsicId::VecNew
            | IntrinsicId::VecWithCapacity
            | IntrinsicId::VecLen
            | IntrinsicId::VecCapacity
            | IntrinsicId::VecIsEmpty
            | IntrinsicId::VecPush
            | IntrinsicId::VecPop
            | IntrinsicId::VecClear
            | IntrinsicId::VecReserve
            | IntrinsicId::VecIndexRead
            | IntrinsicId::VecIndexWrite
            | IntrinsicId::VecPtr
            | IntrinsicId::VecPtrMut
            | IntrinsicId::VecTerminatedPtr
            | IntrinsicId::VecClone
            | IntrinsicId::VecDispose
            | IntrinsicId::VecEq
            | IntrinsicId::VecCmp
            | IntrinsicId::VecContains
            | IntrinsicId::VecStartsWith
            | IntrinsicId::VecEndsWith
            | IntrinsicId::VecConcat
            | IntrinsicId::VecExtendFromSlice => Err(CompileError::new(
                ErrorKind::UnknownIntrinsic(def.name.to_string()),
                span,
            )),
            // ADR-0079 Phase 2b: `@uninit(T)` is recognized by
            // `analyze_alloc` when it appears as a let-init. Reaching
            // this arm means it was used in some other position
            // (returned, passed to a non-`@field_set`/@finalize call,
            // etc.) — that's a misuse.
            IntrinsicId::Uninit => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "@uninit(T) is only valid as the initializer of a `let` binding".into(),
                },
                span,
            )),
            // `@variant_uninit(T, tag)` is similar — only valid as a
            // let-init.
            IntrinsicId::VariantUninit => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason:
                        "@variant_uninit(T, tag) is only valid as the initializer of a `let` binding"
                            .into(),
                },
                span,
            )),
            IntrinsicId::FieldSet => self.analyze_field_set_intrinsic(air, &args, span, ctx),
            IntrinsicId::Finalize => self.analyze_finalize_intrinsic(air, &args, span, ctx),
            IntrinsicId::VariantField => {
                self.analyze_variant_field_intrinsic(air, &args, span, ctx)
            }

            // ADR-0064: build a slice from a raw pointer and a length.
            IntrinsicId::PartsToSlice => {
                self.analyze_parts_to_slice_intrinsic(air, &args, span, ctx, false)
            }
            IntrinsicId::PartsToMutSlice => {
                self.analyze_parts_to_slice_intrinsic(air, &args, span, ctx, true)
            }
            // ADR-0066: @vec(...), @vec_repeat(v, n), @parts_to_vec(p, l, c).
            IntrinsicId::VecLiteral => self.analyze_vec_literal_intrinsic(air, &args, span, ctx),
            IntrinsicId::VecRepeat => self.analyze_vec_repeat_intrinsic(air, &args, span, ctx),
            IntrinsicId::PartsToVec => self.analyze_parts_to_vec_intrinsic(air, &args, span, ctx),
            // ADR-0072: validate UTF-8 of a borrowed Slice(u8). Returns bool.
            IntrinsicId::Utf8Validate => {
                self.analyze_utf8_validate_intrinsic(air, &args, span, ctx)
            }
            // ADR-0072: copy a NUL-terminated C string into a fresh Vec(u8).
            IntrinsicId::CStrToVec => {
                self.analyze_cstr_to_vec_intrinsic(air, &args, span, ctx)
            }
        }
    }

    /// Analyze `@cstr_to_vec(p: Ptr(u8)) -> Vec(u8)` (ADR-0072).
    fn analyze_cstr_to_vec_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        Self::require_checked_for_intrinsic(ctx, "cstr_to_vec", span)?;
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }
        let p = self.analyze_inst(air, args[0].value, ctx)?;
        // Accept Ptr(u8) only.
        let is_ptr_u8 = matches!(p.ty.kind(), TypeKind::PtrConst(_));
        if !is_ptr_u8 && !p.ty.is_error() {
            return Err(CompileError::type_mismatch(
                "Ptr(u8)".to_string(),
                self.format_type_name(p.ty),
                span,
            ));
        }
        let vec_id = self.type_pool.intern_vec_from_type(Type::U8);
        let vec_ty = Type::new_vec(vec_id);
        let args_start = air.add_extra(&[p.air_ref.as_u32()]);
        let name = self.interner.get_or_intern_static("cstr_to_vec");
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 1,
            },
            ty: vec_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, vec_ty))
    }

    /// Analyze `@utf8_validate(s: borrow Slice(u8)) -> bool` (ADR-0072).
    fn analyze_utf8_validate_intrinsic(
        &mut self,
        air: &mut Air,
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
        let s = self.analyze_inst(air, args[0].value, ctx)?;
        // Accept either `Slice(u8)` or `&v[..]` (already a Slice(u8)).
        let is_slice_u8 = matches!(s.ty.kind(), TypeKind::Slice(_) | TypeKind::MutSlice(_));
        if !is_slice_u8 && !s.ty.is_error() {
            return Err(CompileError::type_mismatch(
                "Slice(u8)".to_string(),
                self.format_type_name(s.ty),
                span,
            ));
        }
        let args_start = air.add_extra(&[s.air_ref.as_u32()]);
        let name = self.interner.get_or_intern_static("utf8_validate");
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

    // Helper methods for intrinsic analysis (delegated from analyze_intrinsic_impl)

    fn analyze_dbg_intrinsic(
        &mut self,
        air: &mut Air,
        _inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let mut arg_air_refs = Vec::with_capacity(args.len());
        for arg in args {
            // @dbg observes its arguments without consuming them: if the arg
            // is a variable of an affine type (e.g. String), the load that
            // analyze_inst emits would otherwise mark the variable as moved.
            // Snapshot moved_vars and restore the relevant entry afterwards,
            // mirroring the borrow-self pattern used by Vec methods.
            let root_var = self.extract_root_variable(arg.value);
            let prior_move_state = root_var.and_then(|v| ctx.moved_vars.get(&v).cloned());

            let arg_result = self.analyze_inst(air, arg.value, ctx)?;
            let arg_type = arg_result.ty;

            if let Some(var) = root_var {
                match prior_move_state {
                    Some(state) => {
                        ctx.moved_vars.insert(var, state);
                    }
                    None => {
                        ctx.moved_vars.remove(&var);
                    }
                }
            }

            // Validate type. At runtime, @dbg accepts integers, booleans, and
            // strings. Structs/enums/arrays are rejected (except errors/never,
            // which propagate); String itself is a builtin struct and is
            // recognized via is_builtin_string in codegen — at the sema level
            // we allow structs here and let codegen handle the String case.
            if !arg_type.is_integer()
                && arg_type != Type::BOOL
                && !arg_type.is_struct()
                && !arg_type.is_enum()
                && !arg_type.is_array()
                && !arg_type.is_error()
                && !arg_type.is_never()
            {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: "dbg".to_string(),
                        expected: "integer, bool, or string".to_string(),
                        found: arg_type.name().to_string(),
                    })),
                    span,
                ));
            }

            arg_air_refs.push(arg_result.air_ref.as_u32());
        }

        let args_len = arg_air_refs.len() as u32;
        let args_start = air.add_extra(&arg_air_refs);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: self.known.dbg,
                args_start,
                args_len,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
    }

    fn analyze_cast_intrinsic(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "cast".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        // Get target type from HM inference
        let target_type = Self::get_resolved_type(ctx, inst_ref, span, "@cast intrinsic")?;

        let arg_result = self.analyze_inst(air, args[0].value, ctx)?;
        let source_type = arg_result.ty;

        // Validate source type: must be numeric (integer or float)
        if !source_type.is_numeric() && !source_type.is_error() && !source_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "cast".to_string(),
                    expected: "numeric type".to_string(),
                    found: source_type.name().to_string(),
                })),
                span,
            ));
        }
        // Validate target type: must be numeric (integer or float)
        if !target_type.is_numeric() && !target_type.is_error() && !target_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "cast".to_string(),
                    expected: "numeric target type".to_string(),
                    found: target_type.name().to_string(),
                })),
                span,
            ));
        }

        // Skip cast if types are the same
        if source_type == target_type || source_type.is_error() || source_type.is_never() {
            return Ok(arg_result);
        }

        // If target type couldn't be inferred (unresolved type variable), require annotation
        if target_type.is_error() {
            return Err(CompileError::new(ErrorKind::TypeAnnotationRequired, span));
        }

        // Choose the right instruction based on source/target type categories
        let data = match (source_type.is_integer(), target_type.is_integer()) {
            (true, true) => AirInstData::IntCast {
                value: arg_result.air_ref,
                from_ty: source_type,
            },
            (true, false) => AirInstData::IntToFloat {
                value: arg_result.air_ref,
                from_ty: source_type,
            },
            (false, true) => AirInstData::FloatToInt {
                value: arg_result.air_ref,
                from_ty: source_type,
            },
            (false, false) => AirInstData::FloatCast {
                value: arg_result.air_ref,
                from_ty: source_type,
            },
        };

        let air_ref = air.add_inst(AirInst {
            data,
            ty: target_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, target_type))
    }

    fn analyze_panic_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // @panic takes an optional string message
        if args.len() > 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "panic".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        if args.is_empty() {
            // Panic with no message
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::UnitConst,
                ty: Type::NEVER,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::NEVER));
        }

        // Analyze the message argument
        let arg_result = self.analyze_inst(air, args[0].value, ctx)?;

        let args_start = air.add_extra(&[arg_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: self.known.panic,
                args_start,
                args_len: 1,
            },
            ty: Type::NEVER,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::NEVER))
    }

    fn analyze_assert_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // @assert takes a bool condition and optional message
        if args.is_empty() || args.len() > 2 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "assert".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        let cond_result = self.analyze_inst(air, args[0].value, ctx)?;

        // Build args for AIR
        let mut extra_data = vec![cond_result.air_ref.as_u32()];
        if args.len() > 1 {
            let msg_result = self.analyze_inst(air, args[1].value, ctx)?;
            extra_data.push(msg_result.air_ref.as_u32());
        }

        let args_len = extra_data.len() as u32;
        let args_start = air.add_extra(&extra_data);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: self.known.assert,
                args_start,
                args_len,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
    }

    /// Analyze @test_preview_gate intrinsic.
    fn analyze_test_preview_gate_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // @test_preview_gate() - no-op intrinsic gated by test_infra preview feature.
        self.require_preview(
            PreviewFeature::TestInfra,
            "@test_preview_gate() intrinsic",
            span,
        )?;

        // Takes no arguments
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "test_preview_gate".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        // No-op: just return a unit constant
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::UnitConst,
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
    }

    /// Analyze @compile_error intrinsic.
    ///
    /// In the runtime analysis path, @compile_error is a comptime-only intrinsic
    /// that has type `!` (never). It takes exactly one string literal argument.
    /// The actual error emission happens in the comptime interpreter.
    fn analyze_compile_error_intrinsic(
        &mut self,
        _air: &mut Air,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "compile_error".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        // Pull the message out of the string-literal argument. Reaching
        // `@compile_error` at sema time means the user wrote it
        // someplace the compiler analyzed (e.g. the `then` branch of a
        // `comptime if` whose `cond` evaluated to true at comptime).
        // Surface the user-supplied message as a `ComptimeUserError`.
        let arg_inst = self.rir.get(args[0].value);
        let msg = match &arg_inst.data {
            gruel_rir::InstData::StringConst(spur) => self.interner.resolve(spur).to_string(),
            _ => {
                return Err(CompileError::new(
                    ErrorKind::ComptimeEvaluationFailed {
                        reason: "@compile_error requires a string literal argument".into(),
                    },
                    arg_inst.span,
                ));
            }
        };
        Err(CompileError::new(ErrorKind::ComptimeUserError(msg), span))
    }

    /// Analyze @field(value, field_name) intrinsic.
    ///
    /// Accesses a struct field by comptime-known name. The first argument is a
    /// runtime value of struct type, the second is a comptime_str naming the field.
    /// Resolves at compile time to a FieldGet instruction.
    fn analyze_field_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 2 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "field".to_string(),
                    expected: 2,
                    found: args.len(),
                },
                span,
            ));
        }

        // Arg 1: runtime value of struct type — analyze as a projection base
        // (does not mark the variable as moved, like regular field access)
        let value_result = self.analyze_inst_for_projection(air, args[0].value, ctx)?;
        // ADR-0076: auto-deref `Ref(Struct)` / `MutRef(Struct)` so prelude
        // `derive Clone` (`fn clone(self: Ref(Self))`) can `@field(self, …)`
        // and project into the referent struct without writing an explicit
        // dereference.
        let struct_ty = crate::sema::analyze_ops::unwrap_ref_for_place(self, value_result.ty);

        // Verify the value is a struct type
        let struct_id = match struct_ty.kind() {
            TypeKind::Struct(id) => id,
            _ => {
                return Err(CompileError::new(
                    ErrorKind::ComptimeEvaluationFailed {
                        reason: format!("@field requires a struct value, got {}", struct_ty.name()),
                    },
                    span,
                ));
            }
        };

        // Arg 2: field name — evaluate at comptime to get a comptime_str.
        // Uses evaluate_comptime_expr (not evaluate_comptime_block) to preserve
        // the heap, which may contain data from a comptime_unroll iteration.
        let field_name_val = self.evaluate_comptime_expr(args[1].value, ctx, span)?;
        let field_name = match field_name_val {
            ConstValue::ComptimeStr(str_idx) => match &self.comptime_heap[str_idx as usize] {
                ComptimeHeapItem::String(s) => s.clone(),
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: "@field second argument must be a comptime_str".to_string(),
                        },
                        span,
                    ));
                }
            },
            _ => {
                return Err(CompileError::new(
                    ErrorKind::ComptimeEvaluationFailed {
                        reason: "@field second argument must be a comptime_str".to_string(),
                    },
                    span,
                ));
            }
        };

        // Resolve the field name to a field index
        let struct_def = self.type_pool.struct_def(struct_id);
        let (field_idx, field_def) = struct_def.find_field(&field_name).ok_or_else(|| {
            CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!("struct '{}' has no field '{}'", struct_def.name, field_name),
                },
                span,
            )
        })?;
        let field_ty = field_def.ty;

        // Emit a FieldGet instruction
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::FieldGet {
                base: value_result.air_ref,
                struct_id,
                field_index: field_idx as u32,
            },
            ty: field_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, field_ty))
    }

    /// Analyze @read_line intrinsic.
    fn analyze_read_line_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // @read_line() - reads a line from stdin and returns it as a String.
        // Takes no arguments, returns String.
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "read_line".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        // Get the String type
        let string_type = self.builtin_string_type();

        // Create the intrinsic instruction that returns String
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start: 0, // No args
                args_len: 0,
            },
            ty: string_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, string_type))
    }

    /// Analyze @parse_i32, @parse_i64, @parse_u32, @parse_u64 intrinsics.
    fn analyze_parse_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        intrinsic_name_str: &str,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Expects exactly one argument
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: intrinsic_name_str.to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        // Analyze the argument - String borrows are handled by
        // analyze_inst_for_projection to avoid consuming the String
        let arg_result = self.analyze_inst_for_projection(air, args[0].value, ctx)?;
        let arg_type = arg_result.ty;

        // Argument must be a String
        if !self.is_builtin_string(arg_type) {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: format!("@{}", intrinsic_name_str),
                    expected: "String".to_string(),
                    found: arg_type.name().to_string(),
                })),
                span,
            ));
        }

        // Determine the return type based on the intrinsic name
        let return_type = match intrinsic_name_str {
            "parse_i32" => Type::I32,
            "parse_i64" => Type::I64,
            "parse_u32" => Type::U32,
            "parse_u64" => Type::U64,
            _ => unreachable!(),
        };

        // Encode args into extra array
        let args_start = air.add_extra(&[arg_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 1,
            },
            ty: return_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, return_type))
    }

    /// Analyze @random_u32 intrinsic.
    fn analyze_random_u32_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // @random_u32() - takes no arguments, returns u32
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "random_u32".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        // Create the intrinsic instruction that returns u32
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start: 0, // No args
                args_len: 0,
            },
            ty: Type::U32,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::U32))
    }

    /// Analyze @random_u64 intrinsic.
    fn analyze_random_u64_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // @random_u64() - takes no arguments, returns u64
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "random_u64".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        // Create the intrinsic instruction that returns u64
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start: 0, // No args
                args_len: 0,
            },
            ty: Type::U64,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::U64))
    }

    /// Analyze @import intrinsic.
    ///
    /// This requires the `modules` preview feature and takes a single string literal
    /// Analyze `@embed_file("path")`.
    ///
    /// Reads the file at compile time and emits a `BytesConst` instruction
    /// whose type is `Slice(u8)`. The bytes live in a binary-baked global
    /// at codegen — the slice borrows them with effectively static lifetime.
    /// Path resolution mirrors `@import`: relative to the source file
    /// containing the call, with a fallback to the cwd if the source path
    /// is unknown.
    fn analyze_embed_file_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "embed_file".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        // Argument must be a string literal — we keep diagnostics anchored
        // on the literal and avoid running the comptime interpreter for a
        // file-system side-effect.
        let arg_inst = self.rir.get(args[0].value);
        let path_str = if let gruel_rir::InstData::StringConst(spur) = &arg_inst.data {
            self.interner.resolve(spur).to_string()
        } else {
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "@embed_file requires a string literal argument".to_string(),
                },
                arg_inst.span,
            ));
        };

        // Resolve relative to the calling source file, falling back to cwd.
        use std::path::{Path, PathBuf};
        let resolved: PathBuf = {
            let candidate = Path::new(&path_str);
            if candidate.is_absolute() {
                candidate.to_path_buf()
            } else if let Some(source_path) = self.get_source_path(span) {
                Path::new(source_path)
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(candidate)
            } else {
                candidate.to_path_buf()
            }
        };

        let bytes = std::fs::read(&resolved).map_err(|e| {
            CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!("@embed_file: cannot read '{}': {}", resolved.display(), e),
                },
                span,
            )
        })?;

        let local_id = ctx.add_local_bytes(bytes);

        // Type: `Slice(u8)`. Reuse the slice intern pool used by `@parts_to_slice`.
        let slice_id = self.type_pool.intern_slice_from_type(Type::U8);
        let result_ty = Type::new_slice(slice_id);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::BytesConst(local_id),
            ty: result_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_ty))
    }

    /// argument specifying the module path to import.
    fn analyze_import_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // @import takes exactly one argument
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "import".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        // Accept a string literal (fast path) or a comptime_str expression.
        let import_path = self.resolve_import_path_arg(args[0].value)?;

        // Resolve the import path relative to the current source file
        // Resolution order (per ADR-0026):
        // 1. foo.gruel (simple file module)
        // 2. _foo.gruel with foo/ directory (directory module)
        // 3. (Future) Dependency from gruel.toml
        let resolved_path = self.resolve_import_path(&import_path, span)?;

        // Get or create the module in the registry
        // The module will be populated lazily when member access is performed
        let (module_id, _is_new) = self
            .module_registry
            .get_or_create(import_path.clone(), resolved_path);

        // Return a module type
        // AIR doesn't have a ModuleConst instruction, so we use UnitConst as a placeholder
        // The type is what matters for subsequent member access resolution
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::UnitConst, // Placeholder - module values are compile-time only
            ty: Type::new_module(module_id),
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::new_module(module_id)))
    }

    /// Resolve an import path to an absolute file path.
    ///
    /// Resolution order (per ADR-0026):
    /// 1. Standard library: `@import("std")` resolves to the bundled std library
    /// 2. Pre-loaded files (multi-file compilation)
    /// 3. `foo.gruel` (simple file module)
    /// 4. `_foo.gruel` with `foo/` directory (directory module)
    /// 5. (Future) Dependency from gruel.toml
    pub(crate) fn resolve_import_path(
        &self,
        import_path: &str,
        span: Span,
    ) -> CompileResult<String> {
        use std::path::Path;

        // Phase 0: Check for standard library import
        // @import("std") resolves to the compiler's bundled standard library
        if import_path == "std" {
            return self.resolve_std_import(span);
        }

        // Phase 1: Check if the import path matches an already-loaded file
        // This handles unit tests and multi-file compilation where all files are pre-loaded
        for path in self.file_paths.values() {
            // Check for exact match
            if path == import_path {
                return Ok(path.clone());
            }
            // Suffix match at a path-component boundary. A bare
            // `path.ends_with(import_path)` would mis-match any file whose
            // name happens to share the import's trailing characters: for
            // `@import("string.gruel")` from the prelude, a user file at
            // `scratch/test_make_string.gruel` literally ends with
            // `"string.gruel"` and would incorrectly win over the prelude's
            // own `prelude/string.gruel`. Require the byte before the
            // matched suffix to be `/`.
            if path.len() > import_path.len()
                && path.ends_with(import_path)
                && path.as_bytes()[path.len() - import_path.len() - 1] == b'/'
            {
                return Ok(path.clone());
            }
            // For imports like "math" or "math.gruel", check if the file is named accordingly
            let import_base = import_path.strip_suffix(".gruel").unwrap_or(import_path);
            let file_name = Path::new(path).file_stem().and_then(|s| s.to_str());
            if let Some(name) = file_name
                && name == import_base
            {
                return Ok(path.clone());
            }
        }

        // Phase 2: Try to find the file on disk (for directory modules and actual file imports)
        // Get the directory of the current source file
        let source_path = self.get_source_path(span);
        let source_dir = source_path
            .and_then(|p| Path::new(p).parent())
            .unwrap_or(Path::new("."));

        let mut candidates = Vec::new();

        // Strip .gruel extension if present for base name calculation
        let base_name = import_path.strip_suffix(".gruel").unwrap_or(import_path);

        // Resolution order:
        // 1. Try foo.gruel (simple file module)
        let file_candidate = source_dir.join(format!("{}.gruel", base_name));
        candidates.push(file_candidate.display().to_string());
        if file_candidate.exists() {
            return Ok(file_candidate.to_string_lossy().to_string());
        }

        // 2. If the path already ends in .gruel, also try it directly
        if import_path.ends_with(".gruel") {
            let candidate = source_dir.join(import_path);
            if !candidates.contains(&candidate.display().to_string()) {
                candidates.push(candidate.display().to_string());
            }
            if candidate.exists() {
                return Ok(candidate.to_string_lossy().to_string());
            }
        }

        // 3. Try _foo.gruel + foo/ directory (directory module)
        let dir_module_root = source_dir.join(format!("_{}.gruel", base_name));
        let dir_path = source_dir.join(base_name);
        candidates.push(format!("{} + {}/", dir_module_root.display(), base_name));
        if dir_module_root.exists() && dir_path.is_dir() {
            return Ok(dir_module_root.to_string_lossy().to_string());
        }

        // 3b. Also try just _foo.gruel without requiring foo/ directory
        // (This allows directory modules where all submodules are re-exported)
        if dir_module_root.exists() {
            return Ok(dir_module_root.to_string_lossy().to_string());
        }

        // Module not found - report error with candidates tried
        Err(CompileError::new(
            ErrorKind::ModuleNotFound {
                path: import_path.to_string(),
                candidates,
            },
            span,
        ))
    }

    /// Resolve the standard library import.
    ///
    /// The standard library is located using the following resolution order:
    /// 1. `GRUEL_STD_PATH` environment variable (if set)
    /// 2. `std/` directory relative to the source file
    /// 3. Known installation paths
    ///
    /// Returns the path to `_std.gruel`, the standard library root module.
    fn resolve_std_import(&self, span: Span) -> CompileResult<String> {
        use std::path::Path;

        // Check if we have a pre-loaded std library in file_paths
        for path in self.file_paths.values() {
            // Check for _std.gruel
            if path.ends_with("_std.gruel") || path.ends_with("std/_std.gruel") {
                return Ok(path.clone());
            }
        }

        // 1. Check GRUEL_STD_PATH environment variable
        if let Ok(std_path) = std::env::var("GRUEL_STD_PATH") {
            let std_root = Path::new(&std_path).join("_std.gruel");
            if std_root.exists() {
                return Ok(std_root.to_string_lossy().to_string());
            }
        }

        // 2. Look for std/ relative to the source file
        if let Some(source_path) = self.get_source_path(span) {
            let source_dir = Path::new(source_path).parent().unwrap_or(Path::new("."));

            // Try std/_std.gruel relative to source
            let std_root = source_dir.join("std").join("_std.gruel");
            if std_root.exists() {
                return Ok(std_root.to_string_lossy().to_string());
            }
        }

        // Note: We intentionally do NOT check the current working directory
        // because it's unreliable and may find the wrong std library.
        // Users should either:
        // 1. Set GRUEL_STD_PATH environment variable
        // 2. Have std/ in the same directory as their source files
        // 3. Use aux_files in tests to provide std

        // Standard library not found
        Err(CompileError::new(ErrorKind::StdLibNotFound, span))
    }
    /// Analyze @syscall intrinsic: perform a raw OS syscall.
    /// Signature: @syscall(syscall_num: u64, arg0?: u64, ..., arg5?: u64) -> i64
    ///
    /// Takes a syscall number and up to 6 arguments, all of which must be u64.
    /// Returns i64 (the syscall return value, which may be negative for errors).
    /// Requires a checked block.
    fn analyze_syscall_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Syscall takes 1-7 arguments: syscall number + up to 6 arguments
        if args.is_empty() || args.len() > 7 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "syscall".to_string(),
                    expected: 7, // Show max expected for "at least 1, at most 7"
                    found: args.len(),
                },
                span,
            ));
        }

        // Analyze all arguments and verify they are u64
        let mut arg_refs = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            let arg_result = self.analyze_inst(air, arg.value, ctx)?;
            let arg_type = arg_result.ty;

            // All syscall arguments must be u64
            if arg_type != Type::U64 && !arg_type.is_error() && !arg_type.is_never() {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: "syscall".to_string(),
                        expected: format!("u64 for argument {}", i),
                        found: self.format_type_name(arg_type),
                    })),
                    span,
                ));
            }

            arg_refs.push(arg_result.air_ref.as_u32());
        }

        // Create the intrinsic call instruction
        let args_start = air.add_extra(&arg_refs);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: args.len() as u32,
            },
            ty: Type::I64,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::I64))
    }

    /// Analyze @target_arch() intrinsic - returns target CPU architecture enum.
    ///
    /// This intrinsic takes no arguments and returns an Arch enum value
    /// representing the target CPU architecture (X86_64 or Aarch64).
    fn analyze_target_arch_intrinsic(
        &self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // Validate: no arguments
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "target_arch".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        let arch_enum_id = self
            .builtin_arch_id
            .expect("Arch enum not injected - internal compiler error");

        // Determine variant index based on the *compile* target's
        // architecture (ADR-0077). Cross-compilation reflects here, not
        // the host.
        let variant_index = arch_variant_index(self.target.arch());

        let result_type = Type::new_enum(arch_enum_id);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::EnumVariant {
                enum_id: arch_enum_id,
                variant_index,
            },
            ty: result_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_type))
    }

    /// Analyze @target_os() intrinsic - returns target operating system enum.
    ///
    /// This intrinsic takes no arguments and returns an Os enum value
    /// representing the target operating system (Linux or Macos).
    fn analyze_target_os_intrinsic(
        &self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // Validate: no arguments
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "target_os".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        let os_enum_id = self
            .builtin_os_id
            .expect("Os enum not injected - internal compiler error");

        // Determine variant index based on the *compile* target's OS
        // (ADR-0077). Cross-compilation reflects here, not the host.
        let variant_index = os_variant_index(self.target.os());

        let result_type = Type::new_enum(os_enum_id);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::EnumVariant {
                enum_id: os_enum_id,
                variant_index,
            },
            ty: result_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_type))
    }
}

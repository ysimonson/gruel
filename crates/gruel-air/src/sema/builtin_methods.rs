//! Built-in associated-function and method dispatch.
//!
//! Hosts the analyzers that lower calls on synthetic built-in types
//! (currently `String`) — both associated functions like
//! `String::from_cstr(...)` and instance methods like `s.len()`. The
//! methods here are extensions of `Sema` split out from `analysis.rs`
//! for readability — they are leaf functions called from
//! `analyze_method_call_impl` / `analyze_assoc_fn_call_impl`.

use gruel_builtins::BuiltinTypeDef;
use gruel_rir::{InstData, InstRef, RirCallArg, RirParamMode};
use gruel_util::{CompileError, CompileResult, ErrorKind, Span};

use super::Sema;
use super::context::{
    AnalysisContext, AnalysisResult, BuiltinMethodContext, ReceiverInfo, StringReceiverStorage,
};
use crate::inst::{Air, AirArgMode, AirInst, AirInstData, AirRef};
use crate::types::{StructId, Type};

impl<'a> Sema<'a> {
    /// Analyze a builtin type associated function call.
    ///
    /// Dispatches to the appropriate runtime function based on the builtin registry.
    pub(super) fn analyze_builtin_assoc_fn(
        &mut self,
        air: &mut Air,
        ctx: &mut AnalysisContext,
        (struct_id, builtin_def): (StructId, &'static BuiltinTypeDef),
        function_name: &str,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        use gruel_builtins::{BuiltinParamType, BuiltinReturnType};

        // Look up the associated function in the registry
        let assoc_fn = builtin_def
            .find_associated_fn(function_name)
            .ok_or_else(|| {
                CompileError::new(
                    ErrorKind::UndefinedAssocFn {
                        type_name: builtin_def.name.to_string(),
                        function_name: function_name.to_string(),
                    },
                    span,
                )
            })?;

        // ADR-0072: enforce preview-feature and `checked`-block gates.
        self.check_string_vec_bridge_method_gates(builtin_def.name, function_name, ctx, span)?;

        // Check argument count
        if args.len() != assoc_fn.params.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: assoc_fn.params.len(),
                    found: args.len(),
                },
                span,
            ));
        }

        // Analyze arguments and check types
        let mut air_args: Vec<(AirRef, AirArgMode)> = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            let arg_result = self.analyze_inst(air, arg.value, ctx)?;

            // Get expected type from param
            let expected_ty = match assoc_fn.params[i].ty {
                BuiltinParamType::U64 => Type::U64,
                BuiltinParamType::Usize => Type::USIZE,
                BuiltinParamType::U8 => Type::U8,
                BuiltinParamType::Bool => Type::BOOL,
                BuiltinParamType::Char => Type::CHAR,
                BuiltinParamType::SelfType => Type::new_struct(struct_id),
                BuiltinParamType::BuiltinType(name) => self.resolve_builtin_type_name(name),
            };

            // Type check
            if arg_result.ty != expected_ty && !arg_result.ty.is_error() {
                return Err(CompileError::type_mismatch(
                    expected_ty.name().to_string(),
                    arg_result.ty.name().to_string(),
                    span,
                ));
            }

            air_args.push((arg_result.air_ref, AirArgMode::Normal));
        }

        // Determine return type
        // Use builtin_air_type for SelfType to get correct AIR output type
        let return_ty = match assoc_fn.return_ty {
            BuiltinReturnType::Unit => Type::UNIT,
            BuiltinReturnType::U64 => Type::U64,
            BuiltinReturnType::Usize => Type::USIZE,
            BuiltinReturnType::U8 => Type::U8,
            BuiltinReturnType::Bool => Type::BOOL,
            BuiltinReturnType::SelfType => self.builtin_air_type(struct_id),
            BuiltinReturnType::BuiltinType(name) => self.resolve_builtin_type_name(name),
        };

        // Generate runtime function call
        let call_name = self.interner.get_or_intern(assoc_fn.runtime_fn);

        // Encode args into extra array
        let mut extra_data: Vec<u32> = Vec::with_capacity(air_args.len() * 2);
        for (air_ref, mode) in &air_args {
            extra_data.push(air_ref.as_u32());
            extra_data.push(mode.as_u32());
        }
        let args_start = air.add_extra(&extra_data);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: call_name,
                args_start,
                args_len: air_args.len() as u32,
            },
            ty: return_ty,
            span,
        });

        Ok(AnalysisResult::new(air_ref, return_ty))
    }

    /// Analyze a builtin type method call.
    ///
    /// Dispatches to the appropriate runtime function based on the builtin registry.
    /// Handles borrow semantics (for query methods) and mutation semantics (for
    /// methods that modify the receiver).
    pub(super) fn analyze_builtin_method(
        &mut self,
        air: &mut Air,
        ctx: &mut AnalysisContext,
        method_ctx: &BuiltinMethodContext<'_>,
        receiver: ReceiverInfo,
        args: &[RirCallArg],
    ) -> CompileResult<AnalysisResult> {
        use gruel_builtins::{BuiltinParamType, BuiltinReturnType, ReceiverMode};

        // Look up the method in the registry
        let method = method_ctx
            .builtin_def
            .find_method(method_ctx.method_name)
            .ok_or_else(|| {
                CompileError::new(
                    ErrorKind::UndefinedMethod {
                        type_name: method_ctx.builtin_def.name.to_string(),
                        method_name: method_ctx.method_name.to_string(),
                    },
                    method_ctx.span,
                )
            })?;

        // ADR-0072: enforce preview-feature and `checked`-block gating for the
        // String <-> Vec(u8) bridge surface. The registry has no per-method
        // gate today, so this is applied based on the method name.
        self.check_string_vec_bridge_method_gates(
            method_ctx.builtin_def.name,
            method_ctx.method_name,
            ctx,
            method_ctx.span,
        )?;

        // Handle receiver mode (borrow vs mutation vs consume)
        match method.receiver_mode {
            ReceiverMode::ByRef => {
                // Borrow semantics - "unmove" the variable since it's not consumed
                if let Some(var_symbol) = receiver.var {
                    ctx.moved_vars.remove(&var_symbol);
                }
            }
            ReceiverMode::ByMutRef => {
                // Mutation semantics - variable remains valid after
                if let Some(var_symbol) = receiver.var {
                    ctx.moved_vars.remove(&var_symbol);
                }
            }
            ReceiverMode::ByValue => {
                // Consume semantics - variable is moved (already handled by analyze_inst)
            }
        }

        // Check argument count
        if args.len() != method.params.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: method.params.len(),
                    found: args.len(),
                },
                method_ctx.span,
            ));
        }

        // ADR-0072: `ByMutRef` methods that return a non-`Self` value (e.g.
        // `String::terminated_ptr -> Ptr(u8)`) need to mutate the receiver
        // in place — but the regular `store_string_result` write-back path
        // assumes the call returns the new `Self` value. Pass the receiver
        // as `Inout` (pointer-to-aggregate) so the runtime can mutate it
        // directly, and skip the write-back below.
        let mutates_in_place = matches!(method.receiver_mode, ReceiverMode::ByMutRef)
            && matches!(method.return_ty, BuiltinReturnType::BuiltinType(_));

        // Analyze arguments and check types
        let mut air_args: Vec<(AirRef, AirArgMode)> = Vec::with_capacity(args.len() + 1);

        // Add receiver as first argument. Use `Inout` for in-place mutators
        // so codegen passes a pointer to the receiver storage; otherwise
        // pass by value and rely on the write-back path.
        let recv_mode = if mutates_in_place {
            AirArgMode::MutRef
        } else {
            AirArgMode::Normal
        };
        air_args.push((receiver.result.air_ref, recv_mode));

        // Analyze and add other arguments
        for (i, arg) in args.iter().enumerate() {
            let arg_result = self.analyze_inst(air, arg.value, ctx)?;

            // Get expected type from param
            let expected_ty = match method.params[i].ty {
                BuiltinParamType::U64 => Type::U64,
                BuiltinParamType::Usize => Type::USIZE,
                BuiltinParamType::U8 => Type::U8,
                BuiltinParamType::Bool => Type::BOOL,
                BuiltinParamType::Char => Type::CHAR,
                BuiltinParamType::SelfType => Type::new_struct(method_ctx.struct_id),
                BuiltinParamType::BuiltinType(name) => self.resolve_builtin_type_name(name),
            };

            // Type check
            if arg_result.ty != expected_ty
                && !arg_result.ty.is_error()
                && !(self.is_builtin_string(arg_result.ty)
                    && matches!(method.params[i].ty, BuiltinParamType::SelfType))
            {
                return Err(CompileError::type_mismatch(
                    expected_ty.name().to_string(),
                    arg_result.ty.name().to_string(),
                    method_ctx.span,
                ));
            }

            air_args.push((arg_result.air_ref, AirArgMode::Normal));
        }

        // Determine return type
        // Use builtin_air_type for SelfType to get correct AIR output type
        let return_ty = match method.return_ty {
            BuiltinReturnType::Unit => Type::UNIT,
            BuiltinReturnType::U64 => Type::U64,
            BuiltinReturnType::Usize => Type::USIZE,
            BuiltinReturnType::U8 => Type::U8,
            BuiltinReturnType::Bool => Type::BOOL,
            BuiltinReturnType::SelfType => self.builtin_air_type(method_ctx.struct_id),
            BuiltinReturnType::BuiltinType(name) => self.resolve_builtin_type_name(name),
        };

        // Generate runtime function call
        let call_name = self.interner.get_or_intern(method.runtime_fn);

        // Encode args into extra array
        let mut extra_data: Vec<u32> = Vec::with_capacity(air_args.len() * 2);
        for (air_ref, mode) in &air_args {
            extra_data.push(air_ref.as_u32());
            extra_data.push(mode.as_u32());
        }
        let args_start = air.add_extra(&extra_data);

        let call_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: call_name,
                args_start,
                args_len: air_args.len() as u32,
            },
            ty: return_ty,
            span: method_ctx.span,
        });

        // For mutation methods that return `Self`, store the result back
        // to the receiver. ADR-0072 in-place mutators (e.g.
        // `terminated_ptr`) skip this — the runtime already mutated the
        // receiver via the `Inout` pointer.
        if method.receiver_mode == ReceiverMode::ByMutRef && !mutates_in_place {
            let storage = receiver.storage.ok_or_else(|| {
                CompileError::new(ErrorKind::InvalidAssignmentTarget, method_ctx.span)
            })?;
            return self.store_string_result(air, call_ref, storage, method_ctx.span);
        }

        Ok(AnalysisResult::new(call_ref, return_ty))
    }

    /// Get the storage location for a String receiver in a mutation method call.
    ///
    /// For mutation methods like `push_str`, `push`, `clear`, `reserve`, we need
    /// to know where to store the updated String after the runtime function returns.
    ///
    /// Returns `Some(storage)` if the receiver is a mutable local or inout parameter.
    /// Returns an error if the receiver is:
    /// - An immutable binding (`let` instead of `var`)
    /// - A borrow parameter (can't mutate borrowed values)
    /// - Not an lvalue (e.g., a function call result)
    pub(super) fn get_string_receiver_storage(
        &self,
        receiver_ref: InstRef,
        ctx: &AnalysisContext,
        span: Span,
    ) -> CompileResult<Option<StringReceiverStorage>> {
        let receiver_inst = self.rir.get(receiver_ref);

        match &receiver_inst.data {
            InstData::VarRef { name } => {
                // Check if this is a parameter
                if let Some(param_info) = ctx.params.iter().find(|p| p.name == *name) {
                    // Check parameter mode
                    match param_info.mode {
                        RirParamMode::MutRef => {
                            return Ok(Some(StringReceiverStorage::Param {
                                abi_slot: param_info.abi_slot,
                            }));
                        }
                        RirParamMode::Ref => {
                            let name_str = self.interner.resolve(name);
                            return Err(CompileError::new(
                                ErrorKind::MutateBorrowedValue {
                                    variable: name_str.to_string(),
                                },
                                span,
                            ));
                        }
                        RirParamMode::Normal | RirParamMode::Comptime => {
                            // Normal and comptime parameters are immutable
                            let name_str = self.interner.resolve(name);
                            return Err(CompileError::new(
                                ErrorKind::AssignToImmutable(name_str.to_string()),
                                span,
                            ));
                        }
                    }
                }

                // Check if it's a local variable
                if let Some(local) = ctx.locals.get(name) {
                    if !local.is_mut {
                        let name_str = self.interner.resolve(name);
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(name_str.to_string()),
                            span,
                        ));
                    }
                    return Ok(Some(StringReceiverStorage::Local { slot: local.slot }));
                }

                // Variable not found
                let name_str = self.interner.resolve(name);
                Err(CompileError::new(
                    ErrorKind::UndefinedVariable(name_str.to_string()),
                    span,
                ))
            }

            // For other receiver types (field access, function calls, etc.),
            // we don't support mutation for now
            _ => Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span)),
        }
    }

    /// Store the result of a String mutation method back to the receiver's storage.
    ///
    /// Returns a Unit-typed result since mutation methods don't return a value.
    fn store_string_result(
        &self,
        air: &mut Air,
        call_ref: AirRef,
        storage: StringReceiverStorage,
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        let store_ref = match storage {
            StringReceiverStorage::Local { slot } => air.add_inst(AirInst {
                data: AirInstData::Store {
                    slot,
                    value: call_ref,
                    // The old string value was consumed by the mutation function call
                    // (passed as an argument). No drop is needed here.
                    had_live_value: false,
                },
                ty: Type::UNIT,
                span,
            }),
            StringReceiverStorage::Param { abi_slot } => air.add_inst(AirInst {
                data: AirInstData::ParamStore {
                    param_slot: abi_slot,
                    value: call_ref,
                },
                ty: Type::UNIT,
                span,
            }),
        };

        Ok(AnalysisResult::new(store_ref, Type::UNIT))
    }
}

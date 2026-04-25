//! Generic function specialization pass.
//!
//! This module provides the specialization pass that transforms `CallGeneric`
//! instructions into regular `Call` instructions by:
//!
//! 1. Collecting all `CallGeneric` instructions in the analyzed functions
//! 2. For each unique (func_name, type_args) combination, creating a specialized function
//! 3. Rewriting `CallGeneric` to `Call` with the specialized function name
//!
//! # Architecture
//!
//! The specialization pass runs after semantic analysis but before CFG building.
//! It transforms the AIR in-place and adds new specialized functions to the output.

use std::collections::HashMap;

use gruel_error::{CompileError, CompileResult, ErrorKind};
use gruel_rir::RirParamMode;
use gruel_span::Span;
use lasso::{Spur, ThreadedRodeo};

use crate::inst::{Air, AirInstData};
use crate::sema::{AnalyzedFunction, FunctionInfo, InferenceContext, MethodInfo, Sema, SemaOutput};
use crate::types::{StructId, Type};

/// A key for a specialized function: (base_function_name, type_arguments).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpecializationKey {
    /// Base function name (e.g., "identity")
    pub base_name: Spur,
    /// Type arguments (e.g., [Type::I32])
    pub type_args: Vec<Type>,
}

/// Info about a specialization: the mangled name and the first call site span.
struct SpecializationInfo {
    /// The mangled name for the specialized function.
    mangled_name: Spur,
    /// The span of the first call site (for error reporting if the function doesn't exist).
    call_site_span: Span,
}

/// Perform the specialization pass on the sema output.
///
/// This collects all `CallGeneric` instructions, creates specialized functions,
/// and rewrites calls to point to the specialized versions.
pub fn specialize(
    output: &mut SemaOutput,
    sema: &mut Sema<'_>,
    infer_ctx: &InferenceContext,
    interner: &ThreadedRodeo,
) -> CompileResult<()> {
    // Phase 1: Collect all specialization requests
    let mut specializations: HashMap<SpecializationKey, SpecializationInfo> = HashMap::new();

    for func in &output.functions {
        collect_specializations(&func.air, interner, &mut specializations);
    }

    if specializations.is_empty() {
        // No generic calls, nothing to do
        return Ok(());
    }

    // Build a map from key to just the mangled name for the rewrite phase
    let name_map: HashMap<SpecializationKey, Spur> = specializations
        .iter()
        .map(|(k, v)| (k.clone(), v.mangled_name))
        .collect();

    // Phase 2: Rewrite CallGeneric to Call in all functions
    for func in &mut output.functions {
        rewrite_call_generic(&mut func.air, &name_map);
    }

    // Phase 3: Create specialized function bodies by re-analyzing with type substitution
    for (key, info) in &specializations {
        if let Some(fn_info) = sema.functions.get(&key.base_name) {
            let base_info = *fn_info;
            let specialized_func = create_specialized_function(
                sema,
                infer_ctx,
                key,
                info.mangled_name,
                &base_info,
                interner,
            )?;
            output.functions.push(specialized_func);
            continue;
        }

        // Fall back: the name might refer to a generic method encoded as
        // "StructName.methodName" (ADR-0055 — method-level comptime type
        // params). Try to split and resolve as a method.
        if let Some((struct_id, method_name_sym)) =
            resolve_method_name(sema, interner, key.base_name)
            && let Some(method_info) = sema.methods.get(&(struct_id, method_name_sym)).copied()
        {
            let specialized_func = create_specialized_method(
                sema,
                infer_ctx,
                key,
                info.mangled_name,
                struct_id,
                &method_info,
                interner,
            )?;
            output.functions.push(specialized_func);
            continue;
        }

        let func_name = interner.resolve(&key.base_name);
        return Err(CompileError::new(
            ErrorKind::UndefinedFunction(func_name.to_string()),
            info.call_site_span,
        ));
    }

    Ok(())
}

/// Parse a "StructName.methodName" mangled name into a (StructId, method Spur).
/// Returns None if the name does not match the pattern or the struct is
/// unknown.
fn resolve_method_name(
    sema: &Sema<'_>,
    interner: &ThreadedRodeo,
    name: Spur,
) -> Option<(StructId, Spur)> {
    let name_str = interner.resolve(&name);
    let (struct_str, method_str) = name_str.rsplit_once('.')?;
    let struct_sym = interner.get(struct_str)?;
    let struct_id = *sema.structs.get(&struct_sym)?;
    let method_sym = interner.get(method_str)?;
    Some((struct_id, method_sym))
}

/// Collect all specializations needed from a function's AIR.
fn collect_specializations(
    air: &Air,
    interner: &ThreadedRodeo,
    specializations: &mut HashMap<SpecializationKey, SpecializationInfo>,
) {
    for inst in air.instructions() {
        if let AirInstData::CallGeneric {
            name,
            type_args_start,
            type_args_len,
            ..
        } = &inst.data
        {
            // Extract type arguments using the public accessor
            let type_args: Vec<Type> = air
                .get_extra(*type_args_start, *type_args_len)
                .iter()
                .map(|&encoded| Type::from_u32(encoded))
                .collect();

            let key = SpecializationKey {
                base_name: *name,
                type_args: type_args.clone(),
            };

            specializations.entry(key).or_insert_with(|| {
                // Generate a mangled name for the specialized function
                let base_name = interner.resolve(name);
                let mangled = mangle_specialized_name(base_name, &type_args);
                let mangled_sym = interner.get_or_intern(&mangled);
                SpecializationInfo {
                    mangled_name: mangled_sym,
                    call_site_span: inst.span,
                }
            });
        }
    }
}

/// Rewrite CallGeneric instructions to Call instructions.
fn rewrite_call_generic(air: &mut Air, specializations: &HashMap<SpecializationKey, Spur>) {
    // We need to collect the rewrites first, then apply them.
    // This avoids borrowing issues with the extra array.
    let mut rewrites: Vec<(usize, AirInstData)> = Vec::new();

    for (i, inst) in air.instructions().iter().enumerate() {
        if let AirInstData::CallGeneric {
            name,
            type_args_start,
            type_args_len,
            args_start,
            args_len,
        } = &inst.data
        {
            // Extract type arguments to form the key
            let type_args: Vec<Type> = air
                .get_extra(*type_args_start, *type_args_len)
                .iter()
                .map(|&encoded| Type::from_u32(encoded))
                .collect();

            let key = SpecializationKey {
                base_name: *name,
                type_args,
            };

            if let Some(&specialized_name) = specializations.get(&key) {
                // Rewrite to a regular Call
                let new_data = AirInstData::Call {
                    name: specialized_name,
                    args_start: *args_start,
                    args_len: *args_len,
                };
                rewrites.push((i, new_data));
            }
        }
    }

    // Apply all rewrites
    for (index, new_data) in rewrites {
        air.rewrite_inst_data(index, new_data);
    }
}

/// Generate a mangled name for a specialized function.
///
/// `Type::name()` returns generic placeholders like `"<struct>"` for struct
/// and enum types, which would collide across different structs — so we also
/// append the raw `Type` discriminant, which is unique per type. Primitive
/// types get their normal name for readability.
fn mangle_specialized_name(base_name: &str, type_args: &[Type]) -> String {
    let mut mangled = base_name.to_string();
    for ty in type_args {
        mangled.push_str("__");
        mangled.push_str(ty.name());
        // Disambiguate compound types (structs, enums, arrays) whose
        // `name()` is a generic placeholder.
        mangled.push('#');
        mangled.push_str(&ty.as_u32().to_string());
    }
    mangled
}

/// Create a specialized function by re-analyzing the body with type substitution.
///
/// This builds a type substitution map from the comptime parameters to their concrete
/// type arguments, then re-analyzes the function body with these substitutions.
fn create_specialized_function(
    sema: &mut Sema<'_>,
    infer_ctx: &InferenceContext,
    key: &SpecializationKey,
    specialized_name: Spur,
    base_info: &FunctionInfo,
    interner: &ThreadedRodeo,
) -> CompileResult<AnalyzedFunction> {
    let specialized_name_str = interner.resolve(&specialized_name).to_string();

    // Get parameter data from the arena
    let param_names = sema.param_arena.names(base_info.params);
    let param_types = sema.param_arena.types(base_info.params);
    let param_modes = sema.param_arena.modes(base_info.params);
    let param_comptime = sema.param_arena.comptime(base_info.params);

    // Build the type substitution map: comptime param name -> concrete Type
    let mut type_subst: HashMap<Spur, Type> = HashMap::new();
    let mut type_arg_idx = 0;
    let param_names_owned: Vec<Spur> = param_names.to_vec();
    for (i, is_comptime) in param_comptime.iter().enumerate() {
        if *is_comptime && type_arg_idx < key.type_args.len() {
            type_subst.insert(param_names[i], key.type_args[type_arg_idx]);
            type_arg_idx += 1;
        }
    }

    // ADR-0056: for any comptime param with an interface bound, verify that
    // the supplied concrete type structurally conforms to the interface.
    for p in &param_names_owned {
        if let Some(iid) = sema
            .comptime_interface_bounds
            .get(&(key.base_name, *p))
            .copied()
            && let Some(&concrete) = type_subst.get(p)
        {
            sema.check_conforms(concrete, iid, base_info.span)?;
        }
    }

    // Calculate the return type by substituting type parameters
    let return_type = if base_info.return_type == Type::COMPTIME_TYPE {
        // The return type references a type parameter - substitute it
        type_subst
            .get(&base_info.return_type_sym)
            .copied()
            .unwrap_or(Type::UNIT)
    } else {
        base_info.return_type
    };

    // Build the specialized parameter list by:
    // 1. Filtering out comptime parameters (they're erased at runtime)
    // 2. Substituting type parameters in non-comptime parameter types
    let specialized_params: Vec<(Spur, Type, RirParamMode)> = param_names
        .iter()
        .zip(param_types.iter())
        .zip(param_modes.iter())
        .zip(param_comptime.iter())
        .filter(|(((_, _), _), is_comptime)| !*is_comptime)
        .map(|(((name, ty), mode), _)| {
            // If the type is ComptimeType, look it up in the substitution map
            // The param name's type symbol is stored in param_types as ComptimeType,
            // but we need to find which type param it references.
            // For now, we'll need to look at the original RIR to get the type name.
            let concrete_ty = if *ty == Type::COMPTIME_TYPE {
                // This parameter's type is a type parameter. We need to find which one.
                // The type name in RIR is stored in the param's ty field as a Spur.
                // Unfortunately, we've lost that information by this point.
                // We need to look at the original function in RIR.
                substitute_param_type(sema, base_info, *name, &type_subst).unwrap_or(*ty)
            } else {
                *ty
            };
            (*name, concrete_ty, *mode)
        })
        .collect();

    // Now analyze the function body with the specialized types
    let (
        air,
        num_locals,
        num_param_slots,
        param_modes,
        param_slot_types,
        _warnings,
        _local_strings,
        _ref_fns,
        _ref_meths,
    ) = sema.analyze_specialized_function(
        infer_ctx,
        return_type,
        &specialized_params,
        base_info.body,
        &type_subst,
    )?;

    Ok(AnalyzedFunction {
        name: specialized_name_str,
        air,
        num_locals,
        num_param_slots,
        param_modes,
        param_slot_types,
        is_destructor: false,
    })
}

/// Create a specialized method by re-analyzing the body with the method-
/// level type substitution (ADR-0055).
///
/// Unlike `create_specialized_function`, the synthesized function has a
/// `self` receiver prepended to its parameter list (from the struct's type).
fn create_specialized_method(
    sema: &mut Sema<'_>,
    infer_ctx: &InferenceContext,
    key: &SpecializationKey,
    specialized_name: Spur,
    _struct_id: StructId,
    base_info: &MethodInfo,
    interner: &ThreadedRodeo,
) -> CompileResult<AnalyzedFunction> {
    let specialized_name_str = interner.resolve(&specialized_name).to_string();

    let param_names = sema.param_arena.names(base_info.params).to_vec();
    let param_types = sema.param_arena.types(base_info.params).to_vec();
    let param_modes = sema.param_arena.modes(base_info.params).to_vec();
    let param_comptime = sema.param_arena.comptime(base_info.params).to_vec();

    // Build method-level type substitution from comptime type params ->
    // concrete type args (positional, in the order the comptime params
    // appear).
    let mut type_subst: HashMap<Spur, Type> = HashMap::new();
    let mut type_arg_idx = 0;
    for (i, is_comptime) in param_comptime.iter().enumerate() {
        if *is_comptime && type_arg_idx < key.type_args.len() {
            type_subst.insert(param_names[i], key.type_args[type_arg_idx]);
            type_arg_idx += 1;
        }
    }
    // Methods also need `Self` for the receiver — wire it through so struct-
    // literal expressions `Self { ... }` inside the method body still resolve.
    let self_sym = interner.get_or_intern("Self");
    type_subst.insert(self_sym, base_info.struct_type);

    // ADR-0056: enforce interface bounds on comptime type params at
    // specialization time. The bound table is keyed by "StructName.method";
    // re-derive that key here from `key.base_name` (which already encodes the
    // method-mangled name; the bound was inserted under the unmangled
    // "StructName.method" form, so reconstruct it).
    let owner_for_bounds = key.base_name;
    for p in &param_names {
        if let Some(iid) = sema
            .comptime_interface_bounds
            .get(&(owner_for_bounds, *p))
            .copied()
            && let Some(&concrete) = type_subst.get(p)
        {
            sema.check_conforms(concrete, iid, base_info.span)?;
        }
    }

    // Substitute the return type if it references a method-level type param.
    let return_type = if let Some(&ty) = type_subst.get(&base_info.return_type_sym) {
        ty
    } else if base_info.return_type == Type::COMPTIME_TYPE {
        // Unknown comptime return — fall back to the stored type.
        base_info.return_type
    } else {
        base_info.return_type
    };

    // Build specialized param list: prepend self, drop comptime params,
    // substitute type-param references.
    let mut specialized_params: Vec<(Spur, Type, RirParamMode)> = Vec::new();
    if base_info.has_self {
        let self_val_sym = interner.get_or_intern("self");
        specialized_params.push((self_val_sym, base_info.struct_type, RirParamMode::Normal));
    }
    for i in 0..param_names.len() {
        if param_comptime[i] {
            continue;
        }
        let name = param_names[i];
        let ty = param_types[i];
        let mode = param_modes[i];
        let concrete_ty = if ty == Type::COMPTIME_TYPE {
            substitute_method_param_type(sema, base_info, name, &type_subst).unwrap_or(ty)
        } else {
            ty
        };
        specialized_params.push((name, concrete_ty, mode));
    }

    let (
        air,
        num_locals,
        num_param_slots,
        modes_result,
        param_slot_types,
        _warnings,
        _local_strings,
        _ref_fns,
        _ref_meths,
    ) = sema.analyze_specialized_function(
        infer_ctx,
        return_type,
        &specialized_params,
        base_info.body,
        &type_subst,
    )?;

    Ok(AnalyzedFunction {
        name: specialized_name_str,
        air,
        num_locals,
        num_param_slots,
        param_modes: modes_result,
        param_slot_types,
        is_destructor: false,
    })
}

/// Like `substitute_param_type` but for method bodies: walks the RIR to find
/// the FnDecl matching `base_info.body` and resolves param type refs using
/// `type_subst`.
fn substitute_method_param_type(
    sema: &Sema<'_>,
    base_info: &MethodInfo,
    param_name: Spur,
    type_subst: &HashMap<Spur, Type>,
) -> Option<Type> {
    for (_, inst) in sema.rir.iter() {
        if let gruel_rir::InstData::FnDecl {
            body,
            params_start,
            params_len,
            ..
        } = &inst.data
            && *body == base_info.body
        {
            let params = sema.rir.get_params(*params_start, *params_len);
            for param in params {
                if param.name == param_name
                    && let Some(&concrete) = type_subst.get(&param.ty)
                {
                    return Some(concrete);
                }
            }
        }
    }
    None
}

/// Substitute a parameter's type using the type substitution map.
///
/// This looks up the parameter's type symbol in the original RIR function
/// and substitutes it with the concrete type if it's a type parameter.
fn substitute_param_type(
    sema: &Sema<'_>,
    base_info: &FunctionInfo,
    param_name: Spur,
    type_subst: &HashMap<Spur, Type>,
) -> Option<Type> {
    // Walk up to find the FnDecl that contains this body
    for (_, inst) in sema.rir.iter() {
        if let gruel_rir::InstData::FnDecl {
            body,
            params_start,
            params_len,
            ..
        } = &inst.data
            && *body == base_info.body
        {
            // Found the function declaration
            let params = sema.rir.get_params(*params_start, *params_len);
            for param in params {
                if param.name == param_name {
                    // Found the parameter - get its type symbol
                    // If the type symbol is in our substitution map, use that
                    if let Some(&concrete_ty) = type_subst.get(&param.ty) {
                        return Some(concrete_ty);
                    }
                }
            }
        }
    }

    None
}

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

use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

use gruel_rir::{InstRef, RirParamMode};
use gruel_util::Span;
use gruel_util::{CompileError, CompileResult, ErrorKind};
use lasso::{Spur, ThreadedRodeo};

use crate::inst::{Air, AirInstData};
use crate::param_arena::ParamRange;
use crate::sema::{AnalyzedFunction, FunctionInfo, InferenceContext, MethodInfo, Sema};
use crate::types::{StructId, Type};

/// Function/method references discovered while specializing — feed back
/// into the lazy work queue so reachability stays closed under
/// specialization.
#[derive(Debug, Default)]
pub struct SpecializationRefs {
    pub fns: HashSet<Spur>,
    pub meths: HashSet<(StructId, Spur)>,
}

/// One row in the analyzed-functions accumulator: an analyzed body plus its
/// per-function string and byte literal pools (remapped to global tables
/// later).
pub type AnalyzedRow = (AnalyzedFunction, Vec<String>, Vec<Vec<u8>>);

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

/// Perform the specialization pass on the analyzed-functions accumulator.
///
/// Collects every `CallGeneric` instruction across the analyzed bodies,
/// rewrites them to direct `Call`s by mangled name, and synthesizes the
/// specialized bodies. Iterates until the accumulator is closed: each
/// newly-synthesized body can introduce further `CallGeneric`s
/// (transitively-generic specializations), so we re-collect and re-rewrite
/// until no new keys appear.
///
/// Returns the set of regular function/method references discovered while
/// analyzing the synthesized bodies. The caller feeds these back into the
/// lazy work queue so reachability stays closed under specialization
/// (e.g. `use_greeter[T=Foo]` exposes `Foo.greet` as a reachable method
/// even though `main` only sees a `CallGeneric`).
pub fn specialize(
    functions_with_strings: &mut Vec<AnalyzedRow>,
    name_map: &mut HashMap<SpecializationKey, Spur>,
    sema: &mut Sema<'_>,
    infer_ctx: &InferenceContext,
    interner: &ThreadedRodeo,
) -> CompileResult<SpecializationRefs> {
    let mut accumulated_refs = SpecializationRefs::default();

    loop {
        // Phase 1: collect every CallGeneric across the current accumulator.
        let mut seen: HashMap<SpecializationKey, SpecializationInfo> = HashMap::default();
        for (func, _, _) in functions_with_strings.iter() {
            collect_specializations(&func.air, interner, &mut seen);
        }

        // Take only keys we haven't already specialized in a prior round.
        let new_specs: Vec<(SpecializationKey, SpecializationInfo)> = seen
            .into_iter()
            .filter(|(k, _)| !name_map.contains_key(k))
            .collect();

        if new_specs.is_empty() {
            return Ok(accumulated_refs);
        }

        for (k, info) in &new_specs {
            name_map.insert(k.clone(), info.mangled_name);
        }

        // Phase 2: rewrite CallGeneric → Call across every body. Bodies
        // already rewritten in earlier rounds no longer hold CallGenerics, so
        // walking them is a cheap no-op.
        for (func, _, _) in functions_with_strings.iter_mut() {
            rewrite_call_generic(&mut func.air, &name_map);
        }

        // Phase 3: synthesize the specialized bodies for the new keys.
        for (key, info) in new_specs {
            let base = if let Some(fn_info) = sema.functions.get(&key.base_name).copied() {
                SpecializeBase::function(&fn_info)
            } else if let Some((struct_id, method_sym)) =
                resolve_method_name(sema, interner, key.base_name)
                && let Some(method_info) = sema.methods.get(&(struct_id, method_sym)).copied()
            {
                // ADR-0055: generic method encoded as "StructName.methodName".
                SpecializeBase::method(&method_info)
            } else {
                let func_name = interner.resolve(&key.base_name);
                return Err(CompileError::new(
                    ErrorKind::UndefinedFunction(func_name.to_string()),
                    info.call_site_span,
                ));
            };

            let row = create_specialized(
                sema,
                infer_ctx,
                &key,
                info.mangled_name,
                base,
                interner,
                &mut accumulated_refs,
            )?;
            functions_with_strings.push(row);
        }
    }
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

/// View into the parts of a base function or method that specialization
/// needs. Adapts `FunctionInfo` and `MethodInfo` to one shape so the synthesis
/// logic can stay generic.
struct SpecializeBase {
    params: ParamRange,
    return_type: Type,
    return_type_sym: Spur,
    body: InstRef,
    span: Span,
    /// `Some((struct_type, has_self))` for methods (ADR-0055); `None` for
    /// free functions.
    method: Option<(Type, bool)>,
}

impl SpecializeBase {
    fn function(info: &FunctionInfo) -> Self {
        Self {
            params: info.params,
            return_type: info.return_type,
            return_type_sym: info.return_type_sym,
            body: info.body,
            span: info.span,
            method: None,
        }
    }

    fn method(info: &MethodInfo) -> Self {
        Self {
            params: info.params,
            return_type: info.return_type,
            return_type_sym: info.return_type_sym,
            body: info.body,
            span: info.span,
            method: Some((info.struct_type, info.has_self)),
        }
    }
}

/// Synthesize a specialized function or method by re-analyzing the body with
/// the type substitutions implied by `key.type_args`.
///
/// Comptime params are erased at runtime; references to them are substituted
/// with concrete types via the resulting `type_subst` map. For methods,
/// `Self` is also wired in so `Self { ... }` literals and `Self::Variant`
/// paths resolve, and the receiver is prepended to the parameter list.
///
/// ADR-0055 (method-level comptime type params), ADR-0056 (interface bounds).
fn create_specialized(
    sema: &mut Sema<'_>,
    infer_ctx: &InferenceContext,
    key: &SpecializationKey,
    specialized_name: Spur,
    base: SpecializeBase,
    interner: &ThreadedRodeo,
    refs: &mut SpecializationRefs,
) -> CompileResult<AnalyzedRow> {
    let specialized_name_str = interner.resolve(&specialized_name).to_string();

    let param_names = sema.param_arena.names(base.params).to_vec();
    let param_types = sema.param_arena.types(base.params).to_vec();
    let param_modes = sema.param_arena.modes(base.params).to_vec();
    let param_comptime = sema.param_arena.comptime(base.params).to_vec();

    // Build type substitution: comptime param name -> concrete Type. Methods
    // also bind `Self` to the receiver type.
    let mut type_subst: HashMap<Spur, Type> = HashMap::default();
    let mut type_arg_idx = 0;
    for (i, is_comptime) in param_comptime.iter().enumerate() {
        if *is_comptime && type_arg_idx < key.type_args.len() {
            type_subst.insert(param_names[i], key.type_args[type_arg_idx]);
            type_arg_idx += 1;
        }
    }
    if let Some((struct_type, _)) = base.method {
        let self_sym = interner.get_or_intern("Self");
        type_subst.insert(self_sym, struct_type);
    }

    // ADR-0056: for any comptime param with an interface bound, verify the
    // concrete type structurally conforms. The bound table keys by
    // (owner, param) where owner is the function name or "StructName.method"
    // — both are already encoded in `key.base_name`.
    for p in &param_names {
        if let Some(iid) = sema
            .comptime_interface_bounds
            .get(&(key.base_name, *p))
            .copied()
            && let Some(&concrete) = type_subst.get(p)
        {
            sema.check_conforms(concrete, iid, base.span)?;
        }
    }

    // Substitute the return type if it references a type parameter (or
    // `Self`). Concrete return types (e.g. `i32`) miss the lookup and fall
    // through to the declared type unchanged.
    let return_type = type_subst
        .get(&base.return_type_sym)
        .copied()
        .unwrap_or(base.return_type);

    // Specialized param list: prepend `self` for methods with a receiver,
    // drop comptime params (erased), substitute `ComptimeType` references.
    let mut specialized_params: Vec<(Spur, Type, RirParamMode)> = Vec::new();
    if let Some((struct_type, true)) = base.method {
        let self_val_sym = interner.get_or_intern("self");
        specialized_params.push((self_val_sym, struct_type, RirParamMode::Normal));
    }
    for i in 0..param_names.len() {
        if param_comptime[i] {
            continue;
        }
        let name = param_names[i];
        let ty = param_types[i];
        let mode = param_modes[i];
        let concrete_ty = if ty == Type::COMPTIME_TYPE {
            substitute_param_type(sema, base.body, name, &type_subst).unwrap_or(ty)
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
        local_strings,
        local_bytes,
        ref_fns,
        ref_meths,
    ) = sema.analyze_specialized_function(
        infer_ctx,
        return_type,
        &specialized_params,
        base.body,
        &type_subst,
    )?;

    refs.fns.extend(ref_fns);
    refs.meths.extend(ref_meths);

    let analyzed = AnalyzedFunction {
        name: specialized_name_str,
        air,
        num_locals,
        num_param_slots,
        param_modes: modes_result,
        param_slot_types,
        is_destructor: false,
    };
    Ok((analyzed, local_strings, local_bytes))
}

/// Resolve a type-parameter reference on `param_name` by walking the RIR to
/// find the `FnDecl` whose body matches `body`, then looking up the
/// parameter's source-text type symbol in `type_subst`.
fn substitute_param_type(
    sema: &Sema<'_>,
    body: InstRef,
    param_name: Spur,
    type_subst: &HashMap<Spur, Type>,
) -> Option<Type> {
    for (_, inst) in sema.rir.iter() {
        if let gruel_rir::InstData::FnDecl {
            body: fn_body,
            params_start,
            params_len,
            ..
        } = &inst.data
            && *fn_body == body
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

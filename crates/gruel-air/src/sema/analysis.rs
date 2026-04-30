//! Function body analysis and AIR generation.
//!
//! This module contains the core semantic analysis functionality:
//! - Function analysis (analyze_single_function, analyze_method_function, analyze_destructor_function)
//! - Hindley-Milner type inference (run_type_inference)
//! - RIR to AIR instruction lowering (analyze_inst)
//! - Helper functions for expression analysis
//!
//! # Parallel Analysis
//!
//! Function body analysis is parallelized using rayon. The architecture:
//! 1. Declaration gathering (sequential) builds an immutable `SemaContext`
//! 2. Function jobs are collected describing each function to analyze
//! 3. Jobs are processed in parallel using `par_iter`
//! 4. Results are merged (strings deduplicated, warnings collected)

use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

use gruel_builtins::BuiltinTypeDef;
use gruel_intrinsics::{
    IntrinsicId, PointerKind, PointerOpForm, lookup_by_id, lookup_pointer_method,
};
use gruel_rir::{
    InstData, InstRef, RirArgMode, RirCallArg, RirDirective, RirParamMode, RirPattern,
};
use gruel_target::{Arch, Os};
use gruel_util::{BinOp, Span, UnaryOp};
use gruel_util::{
    CompileError, CompileErrors, CompileResult, CompileWarning, ErrorKind,
    IntrinsicTypeMismatchError, MultiErrorResult, OptionExt, PreviewFeature, WarningKind,
};
use lasso::Spur;
use tracing::info_span;

use super::context::{
    AnalysisContext, AnalysisResult, BuiltinMethodContext, ComptimeHeapItem, ConstValue, ParamInfo,
    ReceiverInfo, StringReceiverStorage,
};
use super::{AnalyzedFunction, InferenceContext, MethodInfo, Sema, SemaOutput};
use crate::inference::{
    Constraint, ConstraintContext, ConstraintGenerator, ParamVarInfo, Unifier, UnifyResult,
};
use crate::inst::{
    Air, AirArgMode, AirCallArg, AirInst, AirInstData, AirPlaceBase, AirProjection, AirRef,
};
use crate::types::{EnumId, EnumVariantDef, StructField, StructId, Type, TypeKind};

/// Span context for comptime evaluation: the outer eval site (used for
/// errors that bubble up through nested calls) and the current
/// instruction's span (used for diagnostics specific to this call).
#[derive(Clone, Copy)]
struct ComptimeSpans {
    outer: Span,
    inst: Span,
}

/// Identifies a pointer-method intrinsic in the registry by its compiler-side
/// `IntrinsicId`, the interned symbol used at the call site, and the
/// human-readable operator name (e.g. `"read"`, `"add"`) used in error
/// messages. All three travel together.
pub(crate) struct PointerOpKind<'a> {
    pub(crate) intrinsic: IntrinsicId,
    pub(crate) name: Spur,
    pub(crate) op_name: &'a str,
}

/// Origin of the pointer value for a pointer-op lowering: either a
/// receiver from a method call (`Some(_), None`) or a left-hand-side type
/// from an associated-function call (`None, Some(_)`).
pub(crate) struct PointerOpOrigin {
    pub(crate) receiver: Option<AnalysisResult>,
    pub(crate) lhs_type: Option<Type>,
}

/// Data describing a method body for analysis.
struct MethodBodySpec<'a> {
    return_type: Spur,
    params: &'a [gruel_rir::RirParam],
    body: InstRef,
    /// The type of `self`, or `None` if this is a static/associated function.
    self_type: Option<Type>,
    /// Receiver mode (`self` / `&self` / `&mut self`). Encoded as
    /// `RirParamMode` (0 = Normal, 1 = Inout, 2 = Borrow). Ignored when
    /// `self_type` is `None`.
    self_mode: u8,
}

/// Result of analyzing a function: analyzed function, warnings, local strings,
/// local byte blobs, referenced functions, and referenced methods.
type AnalyzedFnResult = CompileResult<(
    AnalyzedFunction,
    Vec<CompileWarning>,
    Vec<String>,
    Vec<Vec<u8>>,
    HashSet<Spur>,
    HashSet<(StructId, Spur)>,
)>;

/// Raw analysis output: air, local count, param slots, param modes, param slot types,
/// warnings, local strings, local byte blobs, referenced functions, and referenced methods.
type RawFnAnalysis = CompileResult<(
    Air,
    u32,
    u32,
    Vec<crate::inst::AirParamMode>,
    Vec<Type>,
    Vec<CompileWarning>,
    Vec<String>,
    Vec<Vec<u8>>,
    HashSet<Spur>,
    HashSet<(StructId, Spur)>,
)>;

/// Arguments for [`Sema::register_anon_struct_methods_for_comptime_with_subst`].
struct AnonStructSpec {
    struct_id: StructId,
    struct_type: crate::types::Type,
    methods_start: u32,
    methods_len: u32,
}

/// Main entry point for analyzing all function bodies.
///
/// Called from Sema::analyze_all after declarations are collected.
/// Currently uses the sequential analysis path while the parallel infrastructure
/// is being completed.
///
/// # Parallel Analysis Infrastructure
///
/// The parallel analysis infrastructure is ready but not all instruction types
/// are implemented in `analyze_inst_with_context` yet. Once complete:
/// 1. Build `SemaContext` from `Sema`
/// 2. Collect function jobs with `collect_function_jobs`
/// 3. Process with `par_iter` using `analyze_function_job`
/// 4. Merge with `merge_function_results`
pub(crate) fn analyze_all_function_bodies(mut sema: Sema<'_>) -> MultiErrorResult<SemaOutput> {
    // Use lazy analysis when imports are present (multi-file compilation)
    // This ensures only reachable code is analyzed, per ADR-0026
    if sema.has_imports() {
        analyze_function_bodies_lazy(&mut sema)
    } else {
        // Use eager analysis for single-file compilation (backwards compatibility)
        analyze_all_function_bodies_sequential(&mut sema)
    }
}

/// Sequential analysis path (current implementation).
fn analyze_all_function_bodies_sequential(sema: &mut Sema<'_>) -> MultiErrorResult<SemaOutput> {
    // Build inference context once
    let infer_ctx = sema.build_inference_context();

    // Collect analyzed functions with their local strings.
    let mut functions_with_strings: Vec<(AnalyzedFunction, Vec<String>, Vec<Vec<u8>>)> = Vec::new();
    let mut errors = CompileErrors::new();
    let mut all_warnings = Vec::new();

    // Collect method refs from struct declarations to skip them when analyzing regular functions
    let mut method_refs: HashSet<InstRef> = HashSet::default();
    for (_, inst) in sema.rir.iter() {
        match &inst.data {
            InstData::StructDecl {
                methods_start,
                methods_len,
                ..
            } => {
                let methods = sema.rir.get_inst_refs(*methods_start, *methods_len);
                for method_ref in methods {
                    method_refs.insert(method_ref);
                }
            }
            // Also collect methods from anonymous structs/enums (inside comptime functions)
            InstData::AnonStructType {
                methods_start,
                methods_len,
                ..
            }
            | InstData::AnonEnumType {
                methods_start,
                methods_len,
                ..
            } => {
                if *methods_len > 0 {
                    let methods = sema.rir.get_inst_refs(*methods_start, *methods_len);
                    for method_ref in methods {
                        method_refs.insert(method_ref);
                    }
                }
            }
            _ => {}
        }
    }

    // Analyze regular functions (skip generic functions - they're analyzed during specialization)
    for (inst_ref, inst) in sema.rir.iter() {
        if let InstData::FnDecl {
            name,
            params_start,
            params_len,
            return_type,
            body,
            has_self,
            ..
        } = &inst.data
        {
            if method_refs.contains(&inst_ref) {
                continue;
            }

            // Skip methods (has_self = true) - these are handled elsewhere:
            // - Named struct methods are collected below via StructDecl
            // - Anonymous struct methods are analyzed in the fixed-point loop later
            if *has_self {
                continue;
            }

            // Skip FnDecls that are not in the functions table.
            // These are anonymous struct methods which are analyzed separately.
            if !sema.functions.contains_key(name) {
                continue;
            }

            // Skip generic functions - they'll be analyzed during specialization
            if let Some(fn_info) = sema.functions.get(name)
                && fn_info.is_generic
            {
                continue;
            }

            let fn_name = sema.interner.resolve(name).to_string();
            let params = sema.rir.get_params(*params_start, *params_len);

            match sema.analyze_single_function(
                &infer_ctx,
                &fn_name,
                *return_type,
                &params,
                *body,
                inst.span,
            ) {
                Ok((analyzed, warnings, local_strings, local_bytes, _ref_fns, _ref_meths)) => {
                    functions_with_strings.push((analyzed, local_strings, local_bytes));
                    all_warnings.extend(warnings);
                }
                Err(e) => errors.push(e),
            }
        }
    }

    // Analyze method bodies from struct declarations
    for (_, inst) in sema.rir.iter() {
        if let InstData::StructDecl {
            name: type_name,
            methods_start,
            methods_len,
            ..
        } = &inst.data
        {
            let type_name_str = sema.interner.resolve(type_name).to_string();
            let struct_id = match sema.structs.get(type_name) {
                Some(id) => *id,
                None => {
                    errors.push(CompileError::new(
                        ErrorKind::InternalError(format!(
                            "struct '{}' not found in struct map during method analysis",
                            type_name_str
                        )),
                        inst.span,
                    ));
                    continue;
                }
            };
            let struct_type = Type::new_struct(struct_id);

            let methods = sema.rir.get_inst_refs(*methods_start, *methods_len);
            for method_ref in methods {
                let method_inst = sema.rir.get(method_ref);
                if let InstData::FnDecl {
                    name: method_name,
                    params_start,
                    params_len,
                    return_type,
                    body,
                    has_self,
                    receiver_mode,
                    ..
                } = &method_inst.data
                {
                    let method_name_str = sema.interner.resolve(method_name).to_string();
                    let params = sema.rir.get_params(*params_start, *params_len);

                    let full_name = if *has_self {
                        format!("{}.{}", type_name_str, method_name_str)
                    } else {
                        format!("{}::{}", type_name_str, method_name_str)
                    };

                    match sema.analyze_method_function(
                        &infer_ctx,
                        &full_name,
                        MethodBodySpec {
                            return_type: *return_type,
                            params: &params,
                            body: *body,
                            self_type: has_self.then_some(struct_type),
                            self_mode: *receiver_mode,
                        },
                        method_inst.span,
                    ) {
                        Ok((
                            analyzed,
                            warnings,
                            local_strings,
                            local_bytes,
                            _ref_fns,
                            _ref_meths,
                        )) => {
                            functions_with_strings.push((analyzed, local_strings, local_bytes));
                            all_warnings.extend(warnings);
                        }
                        Err(e) => errors.push(e),
                    }
                }
            }
        }
    }

    // Analyze method bodies from enum declarations (ADR-0053).
    // Mirrors the struct-method loop above.
    for (_, inst) in sema.rir.iter() {
        if let InstData::EnumDecl {
            name: type_name,
            methods_start,
            methods_len,
            ..
        } = &inst.data
        {
            if *methods_len == 0 {
                continue;
            }
            let type_name_str = sema.interner.resolve(type_name).to_string();
            let enum_id = match sema.enums.get(type_name) {
                Some(id) => *id,
                None => {
                    errors.push(CompileError::new(
                        ErrorKind::InternalError(format!(
                            "enum '{}' not found in enum map during method analysis",
                            type_name_str
                        )),
                        inst.span,
                    ));
                    continue;
                }
            };
            let enum_type = Type::new_enum(enum_id);

            let methods = sema.rir.get_inst_refs(*methods_start, *methods_len);
            for method_ref in methods {
                let method_inst = sema.rir.get(method_ref);
                if let InstData::FnDecl {
                    name: method_name,
                    params_start,
                    params_len,
                    return_type,
                    body,
                    has_self,
                    receiver_mode,
                    ..
                } = &method_inst.data
                {
                    let method_name_str = sema.interner.resolve(method_name).to_string();
                    let params = sema.rir.get_params(*params_start, *params_len);

                    let full_name = if *has_self {
                        format!("{}.{}", type_name_str, method_name_str)
                    } else {
                        format!("{}::{}", type_name_str, method_name_str)
                    };

                    match sema.analyze_method_function(
                        &infer_ctx,
                        &full_name,
                        MethodBodySpec {
                            return_type: *return_type,
                            params: &params,
                            body: *body,
                            self_type: has_self.then_some(enum_type),
                            self_mode: *receiver_mode,
                        },
                        method_inst.span,
                    ) {
                        Ok((
                            analyzed,
                            warnings,
                            local_strings,
                            local_bytes,
                            _ref_fns,
                            _ref_meths,
                        )) => {
                            functions_with_strings.push((analyzed, local_strings, local_bytes));
                            all_warnings.extend(warnings);
                        }
                        Err(e) => errors.push(e),
                    }
                }
            }
        }
    }

    // Analyze method bodies attached to host types via `@derive(...)`
    // directives (ADR-0058). Mirrors the inline-method loops above; the
    // host type substitutes for `Self` exactly as it does for inline
    // methods on the host.
    let derive_jobs: Vec<(Spur, Spur, bool, super::DeriveBinding)> = sema
        .derive_bindings
        .iter()
        .map(|b| (b.derive_name, b.host_name, b.host_is_enum, *b))
        .collect();
    for (derive_name, host_name, host_is_enum, _binding) in derive_jobs {
        // Each binding's full method list is captured in `Sema::derives`.
        let dmethods: Vec<crate::sema::info::DeriveMethod> = match sema.derives.get(&derive_name) {
            Some(info) => info.methods.clone(),
            None => continue,
        };
        if host_is_enum {
            let enum_id = match sema.enums.get(&host_name).copied() {
                Some(id) => id,
                None => continue,
            };
            let enum_type = Type::new_enum(enum_id);
            let host_str = sema.type_pool.enum_def(enum_id).name.clone();
            for dm in dmethods {
                let m = sema.rir.get(dm.method_ref);
                let InstData::FnDecl {
                    name: method_name,
                    params_start,
                    params_len,
                    return_type,
                    body,
                    has_self,
                    receiver_mode,
                    ..
                } = &m.data
                else {
                    continue;
                };
                let method_str = sema.interner.resolve(method_name).to_string();
                let params = sema.rir.get_params(*params_start, *params_len);
                let full_name = if *has_self {
                    format!("{}.{}", host_str, method_str)
                } else {
                    format!("{}::{}", host_str, method_str)
                };
                match sema.analyze_method_function(
                    &infer_ctx,
                    &full_name,
                    MethodBodySpec {
                        return_type: *return_type,
                        params: &params,
                        body: *body,
                        self_type: has_self.then_some(enum_type),
                        self_mode: *receiver_mode,
                    },
                    m.span,
                ) {
                    Ok((analyzed, warnings, local_strings, local_bytes, _ref_fns, _ref_meths)) => {
                        functions_with_strings.push((analyzed, local_strings, local_bytes));
                        all_warnings.extend(warnings);
                    }
                    Err(e) => errors.push(e),
                }
            }
        } else {
            let struct_id = match sema.structs.get(&host_name).copied() {
                Some(id) => id,
                None => continue,
            };
            let struct_type = Type::new_struct(struct_id);
            let host_str = sema.type_pool.struct_def(struct_id).name.clone();
            for dm in dmethods {
                let m = sema.rir.get(dm.method_ref);
                let InstData::FnDecl {
                    name: method_name,
                    params_start,
                    params_len,
                    return_type,
                    body,
                    has_self,
                    receiver_mode,
                    ..
                } = &m.data
                else {
                    continue;
                };
                let method_str = sema.interner.resolve(method_name).to_string();
                let params = sema.rir.get_params(*params_start, *params_len);
                let full_name = if *has_self {
                    format!("{}.{}", host_str, method_str)
                } else {
                    format!("{}::{}", host_str, method_str)
                };
                match sema.analyze_method_function(
                    &infer_ctx,
                    &full_name,
                    MethodBodySpec {
                        return_type: *return_type,
                        params: &params,
                        body: *body,
                        self_type: has_self.then_some(struct_type),
                        self_mode: *receiver_mode,
                    },
                    m.span,
                ) {
                    Ok((analyzed, warnings, local_strings, local_bytes, _ref_fns, _ref_meths)) => {
                        functions_with_strings.push((analyzed, local_strings, local_bytes));
                        all_warnings.extend(warnings);
                    }
                    Err(e) => errors.push(e),
                }
            }
        }
    }

    // Analyze inline `fn drop(self)` destructor bodies (ADR-0053 phase 3).
    let inline_drops: Vec<(StructId, InstRef, Span)> = sema
        .inline_struct_drops
        .iter()
        .map(|(sid, (body, span))| (*sid, *body, *span))
        .collect();
    for (struct_id, body, drop_span) in inline_drops {
        let struct_def = sema.type_pool.struct_def(struct_id);
        let type_name_str = struct_def.name.clone();
        let full_name = format!("{}.__drop", type_name_str);
        let struct_type = Type::new_struct(struct_id);

        match sema.analyze_destructor_function(&infer_ctx, &full_name, body, drop_span, struct_type)
        {
            Ok((analyzed, warnings, local_strings, local_bytes, _ref_fns, _ref_meths)) => {
                functions_with_strings.push((analyzed, local_strings, local_bytes));
                all_warnings.extend(warnings);
            }
            Err(e) => errors.push(e),
        }
    }

    // Analyze inline enum destructor bodies (ADR-0053 phase 3b).
    let inline_enum_drops_vec: Vec<(EnumId, InstRef, Span)> = sema
        .inline_enum_drops
        .iter()
        .map(|(eid, (body, span))| (*eid, *body, *span))
        .collect();
    for (enum_id, body, drop_span) in inline_enum_drops_vec {
        let enum_def = sema.type_pool.enum_def(enum_id);
        let type_name_str = enum_def.name.clone();
        let full_name = format!("{}.__drop", type_name_str);
        let enum_type = Type::new_enum(enum_id);

        match sema.analyze_destructor_function(&infer_ctx, &full_name, body, drop_span, enum_type) {
            Ok((analyzed, warnings, local_strings, local_bytes, _ref_fns, _ref_meths)) => {
                functions_with_strings.push((analyzed, local_strings, local_bytes));
                all_warnings.extend(warnings);
            }
            Err(e) => errors.push(e),
        }
    }

    // Analyze destructor bodies
    for (_, inst) in sema.rir.iter() {
        if let InstData::DropFnDecl { type_name, body } = &inst.data {
            let type_name_str = sema.interner.resolve(type_name).to_string();
            let struct_id = match sema.structs.get(type_name) {
                Some(id) => *id,
                None => {
                    errors.push(CompileError::new(
                        ErrorKind::InternalError(format!(
                            "destructor for undefined type '{}' survived validation",
                            type_name_str
                        )),
                        inst.span,
                    ));
                    continue;
                }
            };
            let struct_type = Type::new_struct(struct_id);
            let full_name = format!("{}.__drop", type_name_str);

            match sema.analyze_destructor_function(
                &infer_ctx,
                &full_name,
                *body,
                inst.span,
                struct_type,
            ) {
                Ok((analyzed, warnings, local_strings, local_bytes, _ref_fns, _ref_meths)) => {
                    functions_with_strings.push((analyzed, local_strings, local_bytes));
                    all_warnings.extend(warnings);
                }
                Err(e) => errors.push(e),
            }
        }
    }

    // Analyze methods for anonymous structs.
    // These are registered during comptime evaluation of function bodies, so they
    // aren't in any named StructDecl. We use a fixed-point loop since analyzing one
    // method may create new anonymous struct types with their own methods.
    let mut analyzed_anon_methods: HashSet<(StructId, Spur)> = HashSet::default();
    let mut analyzed_anon_enum_methods: HashSet<(EnumId, Spur)> = HashSet::default();
    loop {
        // Collect anonymous struct methods that haven't been analyzed yet
        let pending_anon_methods: Vec<(StructId, Spur, MethodInfo)> = sema
            .methods
            .iter()
            .filter_map(|((struct_id, method_name), method_info)| {
                // Check if this is an anonymous struct
                let struct_def = sema.type_pool.struct_def(*struct_id);
                if struct_def.name.starts_with("__anon_struct_")
                    && !analyzed_anon_methods.contains(&(*struct_id, *method_name))
                {
                    Some((*struct_id, *method_name, *method_info))
                } else {
                    None
                }
            })
            .collect();

        // Collect anonymous enum methods that haven't been analyzed yet
        let pending_anon_enum_methods: Vec<(EnumId, Spur, MethodInfo)> = sema
            .enum_methods
            .iter()
            .filter_map(|((enum_id, method_name), method_info)| {
                let enum_def = sema.type_pool.enum_def(*enum_id);
                if enum_def.name.starts_with("__anon_enum_")
                    && !analyzed_anon_enum_methods.contains(&(*enum_id, *method_name))
                {
                    Some((*enum_id, *method_name, *method_info))
                } else {
                    None
                }
            })
            .collect();

        if pending_anon_methods.is_empty() && pending_anon_enum_methods.is_empty() {
            break;
        }

        for (struct_id, method_name, method_info) in pending_anon_methods {
            analyzed_anon_methods.insert((struct_id, method_name));

            // Skip method-generic methods (method-level comptime type params).
            // Their bodies are analyzed at specialization time, not here —
            // the declared types of their params are still abstract
            // placeholders until a concrete call site supplies the type args.
            if method_info.is_generic {
                continue;
            }

            let struct_def = sema.type_pool.struct_def(struct_id);
            let type_name_str = struct_def.name.clone();
            let method_name_str = sema.interner.resolve(&method_name).to_string();

            let full_name = if method_info.has_self {
                format!("{}.{}", type_name_str, method_name_str)
            } else {
                format!("{}::{}", type_name_str, method_name_str)
            };

            // Build param_info from MethodInfo's ParamRange
            let param_names = sema.param_arena.names(method_info.params);
            let param_types = sema.param_arena.types(method_info.params);
            let param_modes = sema.param_arena.modes(method_info.params);

            let mut param_info: Vec<(Spur, Type, RirParamMode)> = Vec::new();

            if method_info.has_self {
                // Add self parameter (Normal mode - passed by value)
                let self_sym = sema.interner.get_or_intern("self");
                param_info.push((self_sym, method_info.struct_type, RirParamMode::Normal));
            }

            // Add regular parameters (convert from arena slices)
            for i in 0..param_names.len() {
                param_info.push((param_names[i], param_types[i], param_modes[i]));
            }

            // Retrieve captured comptime values from struct-level storage
            // Clone the HashMap to avoid borrowing issues with mutable analyze_method_body call
            let struct_id = method_info
                .struct_type
                .as_struct()
                .expect("method must belong to struct");
            let captured_values = sema
                .anon_struct_captured_values
                .get(&struct_id)
                .cloned()
                .unwrap_or_else(HashMap::default);

            match sema.analyze_method_body(
                &infer_ctx,
                method_info.return_type,
                &param_info,
                method_info.body,
                method_info.struct_type,
                &captured_values,
            ) {
                Ok((
                    air,
                    num_locals,
                    num_param_slots,
                    param_modes_result,
                    param_slot_types,
                    warnings,
                    local_strings,
                    local_bytes,
                    _ref_fns,
                    _ref_meths,
                )) => {
                    let analyzed = AnalyzedFunction {
                        name: full_name,
                        air,
                        num_locals,
                        num_param_slots,
                        param_modes: param_modes_result,
                        param_slot_types,
                        is_destructor: false,
                    };
                    functions_with_strings.push((analyzed, local_strings, local_bytes));
                    all_warnings.extend(warnings);
                }
                Err(e) => errors.push(e),
            }
        }

        // Process anonymous enum methods in the same fixed-point loop
        for (enum_id, method_name, method_info) in pending_anon_enum_methods {
            analyzed_anon_enum_methods.insert((enum_id, method_name));

            let enum_def = sema.type_pool.enum_def(enum_id);
            let type_name_str = enum_def.name.clone();
            let method_name_str = sema.interner.resolve(&method_name).to_string();

            let full_name = if method_info.has_self {
                format!("{}.{}", type_name_str, method_name_str)
            } else {
                format!("{}::{}", type_name_str, method_name_str)
            };

            let param_names = sema.param_arena.names(method_info.params);
            let param_types = sema.param_arena.types(method_info.params);
            let param_modes = sema.param_arena.modes(method_info.params);

            let mut param_info: Vec<(Spur, Type, RirParamMode)> = Vec::new();

            if method_info.has_self {
                let self_sym = sema.interner.get_or_intern("self");
                param_info.push((self_sym, method_info.struct_type, RirParamMode::Normal));
            }

            for i in 0..param_names.len() {
                param_info.push((param_names[i], param_types[i], param_modes[i]));
            }

            let captured_values = sema
                .anon_enum_captured_values
                .get(&enum_id)
                .cloned()
                .unwrap_or_else(HashMap::default);

            match sema.analyze_method_body(
                &infer_ctx,
                method_info.return_type,
                &param_info,
                method_info.body,
                method_info.struct_type,
                &captured_values,
            ) {
                Ok((
                    air,
                    num_locals,
                    num_param_slots,
                    param_modes_result,
                    param_slot_types,
                    warnings,
                    local_strings,
                    local_bytes,
                    _ref_fns,
                    _ref_meths,
                )) => {
                    let analyzed = AnalyzedFunction {
                        name: full_name,
                        air,
                        num_locals,
                        num_param_slots,
                        param_modes: param_modes_result,
                        param_slot_types,
                        is_destructor: false,
                    };
                    functions_with_strings.push((analyzed, local_strings, local_bytes));
                    all_warnings.extend(warnings);
                }
                Err(e) => errors.push(e),
            }
        }
    }

    // Merge strings from all functions into a global table with deduplication.
    // Bytes pools are concatenated (no dedup) — each `@embed_file` call gets
    // a fresh entry since these are rare and may legitimately repeat.
    let mut global_string_table: HashMap<String, u32> = HashMap::default();
    let mut global_strings: Vec<String> = Vec::new();
    let mut global_bytes: Vec<Vec<u8>> = Vec::new();

    let mut functions: Vec<AnalyzedFunction> = Vec::new();
    for (mut analyzed, local_strings, local_bytes) in functions_with_strings {
        if !local_strings.is_empty() {
            let local_to_global: Vec<u32> = local_strings
                .into_iter()
                .map(|s| {
                    *global_string_table.entry(s.clone()).or_insert_with(|| {
                        let id = global_strings.len() as u32;
                        global_strings.push(s);
                        id
                    })
                })
                .collect();

            analyzed
                .air
                .remap_string_ids(|local_id| local_to_global[local_id as usize]);
        }
        if !local_bytes.is_empty() {
            let bytes_offset = global_bytes.len() as u32;
            global_bytes.extend(local_bytes);
            analyzed
                .air
                .remap_bytes_ids(|local_id| local_id + bytes_offset);
        }
        functions.push(analyzed);
    }

    // Emit warnings for any comptime @dbg calls that occurred during comptime evaluation.
    for (msg, span) in std::mem::take(&mut sema.comptime_log_output) {
        all_warnings.push(CompileWarning::new(
            WarningKind::ComptimeDbgPresent(msg),
            span,
        ));
    }

    all_warnings.sort_by_key(|w| w.span().map(|s| s.start));

    let mut output = SemaOutput {
        functions,
        strings: global_strings,
        bytes: global_bytes,
        warnings: all_warnings,
        type_pool: sema.type_pool.clone(),
        comptime_dbg_output: std::mem::take(&mut sema.comptime_dbg_output),
        interface_defs: sema.interface_defs.clone(),
        interface_vtables: sema.interface_vtables_needed.clone(),
    };

    // Run specialization pass to rewrite CallGeneric instructions to Call
    // and create specialized function bodies
    if let Err(e) = crate::specialize::specialize(&mut output, sema, &infer_ctx, sema.interner) {
        errors.push(e);
    }

    // Surface any errors raised during anonymous-host derive expansion
    // (ADR-0058). The comptime evaluator has an `Option` return type so
    // it cannot propagate these via `?`; we collect them here.
    for e in std::mem::take(&mut sema.pending_anon_derive_errors) {
        errors.push(e);
    }

    errors.into_result_with(output)
}

/// Lazy analysis path (Phase 3 of module system, ADR-0026).
///
/// This implements "lazy semantic analysis" where only functions reachable from
/// the entry point (main) are analyzed. Unreferenced code is not analyzed,
/// not codegen'd, and errors in unreferenced code are not reported.
///
/// This is the same trade-off Zig makes for faster builds and smaller binaries.
fn analyze_function_bodies_lazy(sema: &mut Sema<'_>) -> MultiErrorResult<SemaOutput> {
    // Build inference context once
    let infer_ctx = sema.build_inference_context();

    // Find main() function - this is the entry point for lazy analysis
    let main_sym = match sema.interner.get("main") {
        Some(sym) if sema.functions.contains_key(&sym) => sym,
        _ => {
            // No main function found - this is an error
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::NoMainFunction,
            )));
        }
    };

    // Work queue: functions/methods to analyze
    // Start with main()
    let mut pending_functions: Vec<Spur> = vec![main_sym];
    let mut analyzed_functions: HashSet<Spur> = HashSet::default();
    let mut pending_methods: Vec<(StructId, Spur)> = Vec::new();
    let mut analyzed_methods: HashSet<(StructId, Spur)> = HashSet::default();

    // Collect results
    let mut functions_with_strings: Vec<(AnalyzedFunction, Vec<String>, Vec<Vec<u8>>)> = Vec::new();
    let mut errors = CompileErrors::new();
    let mut all_warnings = Vec::new();

    // Collect method refs from struct declarations (for later lookup)
    let mut method_refs: HashSet<InstRef> = HashSet::default();
    for (_, inst) in sema.rir.iter() {
        if let InstData::StructDecl {
            methods_start,
            methods_len,
            ..
        } = &inst.data
        {
            let methods = sema.rir.get_inst_refs(*methods_start, *methods_len);
            for method_ref in methods {
                method_refs.insert(method_ref);
            }
        }
    }

    // Process work queue until empty
    while !pending_functions.is_empty() || !pending_methods.is_empty() {
        // Process pending functions
        while let Some(fn_name) = pending_functions.pop() {
            if analyzed_functions.contains(&fn_name) {
                continue;
            }
            analyzed_functions.insert(fn_name);

            // Look up the function info
            let fn_info = match sema.functions.get(&fn_name) {
                Some(info) => *info,
                None => continue, // Should not happen, but be defensive
            };

            // Skip generic functions - they're analyzed during specialization
            if fn_info.is_generic {
                continue;
            }

            let fn_name_str = sema.interner.resolve(&fn_name).to_string();

            // Find the function declaration in RIR to get params
            let mut found = false;
            for (inst_ref, inst) in sema.rir.iter() {
                if let InstData::FnDecl {
                    name,
                    params_start,
                    params_len,
                    return_type,
                    body,
                    ..
                } = &inst.data
                    && *name == fn_name
                    && !method_refs.contains(&inst_ref)
                {
                    found = true;
                    let params = sema.rir.get_params(*params_start, *params_len);

                    match sema.analyze_single_function(
                        &infer_ctx,
                        &fn_name_str,
                        *return_type,
                        &params,
                        *body,
                        inst.span,
                    ) {
                        Ok((
                            analyzed,
                            warnings,
                            local_strings,
                            local_bytes,
                            referenced_fns,
                            referenced_meths,
                        )) => {
                            functions_with_strings.push((analyzed, local_strings, local_bytes));
                            all_warnings.extend(warnings);

                            // Add newly referenced functions to the work queue
                            for ref_fn in referenced_fns {
                                if !analyzed_functions.contains(&ref_fn) {
                                    pending_functions.push(ref_fn);
                                }
                            }
                            for ref_meth in referenced_meths {
                                if !analyzed_methods.contains(&ref_meth) {
                                    pending_methods.push(ref_meth);
                                }
                            }
                        }
                        Err(e) => errors.push(e),
                    }
                    break;
                }
            }

            if !found {
                // This could be a builtin or otherwise non-existent function
                // Just skip it
            }
        }

        // Process pending methods
        while let Some((struct_id, method_name)) = pending_methods.pop() {
            if analyzed_methods.contains(&(struct_id, method_name)) {
                continue;
            }
            analyzed_methods.insert((struct_id, method_name));

            // Look up the method info
            let method_info = match sema.methods.get(&(struct_id, method_name)) {
                Some(info) => *info,
                None => continue,
            };

            // Method-level generic: defer body analysis to specialization.
            if method_info.is_generic {
                continue;
            }

            // Get the struct definition to find its name for impl block lookup
            let struct_def = sema.type_pool.struct_def(struct_id);
            let type_name_str = struct_def.name.clone();
            let type_name_sym = sema.interner.get_or_intern(&type_name_str);
            let method_name_str = sema.interner.resolve(&method_name).to_string();

            // For anonymous structs, use the MethodInfo directly since there's no named StructDecl
            if type_name_str.starts_with("__anon_struct_") {
                let full_name = if method_info.has_self {
                    format!("{}.{}", type_name_str, method_name_str)
                } else {
                    format!("{}::{}", type_name_str, method_name_str)
                };

                // Build param_info from MethodInfo's ParamRange
                let param_names = sema.param_arena.names(method_info.params);
                let param_types = sema.param_arena.types(method_info.params);
                let param_modes = sema.param_arena.modes(method_info.params);

                let mut param_info: Vec<(Spur, Type, RirParamMode)> = Vec::new();

                if method_info.has_self {
                    // Add self parameter (Normal mode - passed by value)
                    let self_sym = sema.interner.get_or_intern("self");
                    param_info.push((self_sym, method_info.struct_type, RirParamMode::Normal));
                }

                // Add regular parameters (convert from arena slices)
                for i in 0..param_names.len() {
                    param_info.push((param_names[i], param_types[i], param_modes[i]));
                }

                // Retrieve captured comptime values from struct-level storage
                // Clone the HashMap to avoid borrowing issues with mutable analyze_method_body call
                let struct_id = method_info
                    .struct_type
                    .as_struct()
                    .expect("method must belong to struct");
                let captured_values = sema
                    .anon_struct_captured_values
                    .get(&struct_id)
                    .cloned()
                    .unwrap_or_else(HashMap::default);

                match sema.analyze_method_body(
                    &infer_ctx,
                    method_info.return_type,
                    &param_info,
                    method_info.body,
                    method_info.struct_type,
                    &captured_values,
                ) {
                    Ok((
                        air,
                        num_locals,
                        num_param_slots,
                        param_modes_result,
                        param_slot_types,
                        warnings,
                        local_strings,
                        local_bytes,
                        referenced_fns,
                        referenced_meths,
                    )) => {
                        let analyzed = AnalyzedFunction {
                            name: full_name,
                            air,
                            num_locals,
                            num_param_slots,
                            param_modes: param_modes_result,
                            param_slot_types,
                            is_destructor: false,
                        };
                        functions_with_strings.push((analyzed, local_strings, local_bytes));
                        all_warnings.extend(warnings);

                        for ref_fn in referenced_fns {
                            if !analyzed_functions.contains(&ref_fn) {
                                pending_functions.push(ref_fn);
                            }
                        }
                        for ref_meth in referenced_meths {
                            if !analyzed_methods.contains(&ref_meth) {
                                pending_methods.push(ref_meth);
                            }
                        }
                    }
                    Err(e) => errors.push(e),
                }
                continue;
            }

            // Find the method in struct declarations (for named structs)
            for (_, inst) in sema.rir.iter() {
                if let InstData::StructDecl {
                    name: struct_name,
                    methods_start,
                    methods_len,
                    ..
                } = &inst.data
                {
                    if *struct_name != type_name_sym {
                        continue;
                    }

                    let methods = sema.rir.get_inst_refs(*methods_start, *methods_len);
                    for method_ref in methods {
                        let method_inst = sema.rir.get(method_ref);
                        if let InstData::FnDecl {
                            name: m_name,
                            params_start,
                            params_len,
                            return_type,
                            body,
                            has_self,
                            receiver_mode,
                            ..
                        } = &method_inst.data
                        {
                            if *m_name != method_name {
                                continue;
                            }

                            let params = sema.rir.get_params(*params_start, *params_len);
                            let full_name = if *has_self {
                                format!("{}.{}", type_name_str, method_name_str)
                            } else {
                                format!("{}::{}", type_name_str, method_name_str)
                            };

                            match sema.analyze_method_function(
                                &infer_ctx,
                                &full_name,
                                MethodBodySpec {
                                    return_type: *return_type,
                                    params: &params,
                                    body: *body,
                                    self_type: has_self.then_some(method_info.struct_type),
                                    self_mode: *receiver_mode,
                                },
                                method_inst.span,
                            ) {
                                Ok((
                                    analyzed,
                                    warnings,
                                    local_strings,
                                    local_bytes,
                                    referenced_fns,
                                    referenced_meths,
                                )) => {
                                    functions_with_strings.push((
                                        analyzed,
                                        local_strings,
                                        local_bytes,
                                    ));
                                    all_warnings.extend(warnings);

                                    for ref_fn in referenced_fns {
                                        if !analyzed_functions.contains(&ref_fn) {
                                            pending_functions.push(ref_fn);
                                        }
                                    }
                                    for ref_meth in referenced_meths {
                                        if !analyzed_methods.contains(&ref_meth) {
                                            pending_methods.push(ref_meth);
                                        }
                                    }
                                }
                                Err(e) => errors.push(e),
                            }
                        }
                    }
                }
            }
        }
    }

    // Analyze anonymous enum methods that were registered during comptime evaluation.
    // These are not tracked by the work queue (which only handles struct methods),
    // so we process them in a fixed-point loop similar to the eager path.
    let mut analyzed_anon_enum_methods: HashSet<(EnumId, Spur)> = HashSet::default();
    loop {
        let pending_anon_enum_methods: Vec<(EnumId, Spur, MethodInfo)> = sema
            .enum_methods
            .iter()
            .filter_map(|((enum_id, method_name), method_info)| {
                let enum_def = sema.type_pool.enum_def(*enum_id);
                if enum_def.name.starts_with("__anon_enum_")
                    && !analyzed_anon_enum_methods.contains(&(*enum_id, *method_name))
                {
                    Some((*enum_id, *method_name, *method_info))
                } else {
                    None
                }
            })
            .collect();

        if pending_anon_enum_methods.is_empty() {
            break;
        }

        for (enum_id, method_name, method_info) in pending_anon_enum_methods {
            analyzed_anon_enum_methods.insert((enum_id, method_name));

            let enum_def = sema.type_pool.enum_def(enum_id);
            let type_name_str = enum_def.name.clone();
            let method_name_str = sema.interner.resolve(&method_name).to_string();

            let full_name = if method_info.has_self {
                format!("{}.{}", type_name_str, method_name_str)
            } else {
                format!("{}::{}", type_name_str, method_name_str)
            };

            let param_names = sema.param_arena.names(method_info.params);
            let param_types = sema.param_arena.types(method_info.params);
            let param_modes = sema.param_arena.modes(method_info.params);

            let mut param_info: Vec<(Spur, Type, RirParamMode)> = Vec::new();

            if method_info.has_self {
                let self_sym = sema.interner.get_or_intern("self");
                param_info.push((self_sym, method_info.struct_type, RirParamMode::Normal));
            }

            for i in 0..param_names.len() {
                param_info.push((param_names[i], param_types[i], param_modes[i]));
            }

            let captured_values = sema
                .anon_enum_captured_values
                .get(&enum_id)
                .cloned()
                .unwrap_or_else(HashMap::default);

            match sema.analyze_method_body(
                &infer_ctx,
                method_info.return_type,
                &param_info,
                method_info.body,
                method_info.struct_type,
                &captured_values,
            ) {
                Ok((
                    air,
                    num_locals,
                    num_param_slots,
                    param_modes_result,
                    param_slot_types,
                    warnings,
                    local_strings,
                    local_bytes,
                    _ref_fns,
                    _ref_meths,
                )) => {
                    let analyzed = AnalyzedFunction {
                        name: full_name,
                        air,
                        num_locals,
                        num_param_slots,
                        param_modes: param_modes_result,
                        param_slot_types,
                        is_destructor: false,
                    };
                    functions_with_strings.push((analyzed, local_strings, local_bytes));
                    all_warnings.extend(warnings);
                }
                Err(e) => errors.push(e),
            }
        }
    }

    // Also analyze destructors for any structs whose types we've used
    // (This is necessary because drop is implicitly called)
    for (_, inst) in sema.rir.iter() {
        if let InstData::DropFnDecl { type_name, body } = &inst.data {
            let type_name_str = sema.interner.resolve(type_name).to_string();
            let struct_id = match sema.structs.get(type_name) {
                Some(id) => *id,
                None => continue,
            };
            let struct_type = Type::new_struct(struct_id);
            let full_name = format!("{}.__drop", type_name_str);

            match sema.analyze_destructor_function(
                &infer_ctx,
                &full_name,
                *body,
                inst.span,
                struct_type,
            ) {
                Ok((analyzed, warnings, local_strings, local_bytes, _, _)) => {
                    functions_with_strings.push((analyzed, local_strings, local_bytes));
                    all_warnings.extend(warnings);
                }
                Err(e) => errors.push(e),
            }
        }
    }

    // Merge strings from all functions into a global table with deduplication.
    // Bytes pools are concatenated (no dedup) — each `@embed_file` call gets
    // a fresh entry since these are rare and may legitimately repeat.
    let mut global_string_table: HashMap<String, u32> = HashMap::default();
    let mut global_strings: Vec<String> = Vec::new();
    let mut global_bytes: Vec<Vec<u8>> = Vec::new();

    let mut functions: Vec<AnalyzedFunction> = Vec::new();
    for (mut analyzed, local_strings, local_bytes) in functions_with_strings {
        if !local_strings.is_empty() {
            let local_to_global: Vec<u32> = local_strings
                .into_iter()
                .map(|s| {
                    *global_string_table.entry(s.clone()).or_insert_with(|| {
                        let id = global_strings.len() as u32;
                        global_strings.push(s);
                        id
                    })
                })
                .collect();

            analyzed
                .air
                .remap_string_ids(|local_id| local_to_global[local_id as usize]);
        }
        if !local_bytes.is_empty() {
            let bytes_offset = global_bytes.len() as u32;
            global_bytes.extend(local_bytes);
            analyzed
                .air
                .remap_bytes_ids(|local_id| local_id + bytes_offset);
        }
        functions.push(analyzed);
    }

    // Emit warnings for any comptime @dbg calls that occurred during comptime evaluation.
    for (msg, span) in std::mem::take(&mut sema.comptime_log_output) {
        all_warnings.push(CompileWarning::new(
            WarningKind::ComptimeDbgPresent(msg),
            span,
        ));
    }

    all_warnings.sort_by_key(|w| w.span().map(|s| s.start));

    let mut output = SemaOutput {
        functions,
        strings: global_strings,
        bytes: global_bytes,
        warnings: all_warnings,
        type_pool: sema.type_pool.clone(),
        comptime_dbg_output: std::mem::take(&mut sema.comptime_dbg_output),
        interface_defs: sema.interface_defs.clone(),
        interface_vtables: sema.interface_vtables_needed.clone(),
    };

    // Run specialization pass to rewrite CallGeneric instructions to Call
    // and create specialized function bodies
    if let Err(e) = crate::specialize::specialize(&mut output, sema, &infer_ctx, sema.interner) {
        errors.push(e);
    }

    // Surface anonymous-host derive expansion errors (ADR-0058) for the
    // lazy path, mirroring the sequential path above.
    for e in std::mem::take(&mut sema.pending_anon_derive_errors) {
        errors.push(e);
    }

    errors.into_result_with(output)
}

// ============================================================================
// Helper functions for parallel analysis (using SemaContext)
// ============================================================================

impl<'a> Sema<'a> {
    /// Check that a preview feature is enabled.
    ///
    /// This is used to gate experimental features behind the `--preview` flag.
    /// Returns an error with a helpful message if the feature is not enabled.
    ///
    /// # Arguments
    /// - `feature`: The preview feature to check
    /// - `what`: Human-readable description of what requires this feature
    /// - `span`: The source location where the feature is used
    ///
    /// # Returns
    /// - `Ok(())` if the feature is enabled
    /// - `Err(CompileError)` with a helpful message if not enabled
    pub(crate) fn require_preview(
        &self,
        feature: PreviewFeature,
        what: &str,
        span: Span,
    ) -> CompileResult<()> {
        if self.preview_features.contains(&feature) {
            Ok(())
        } else {
            Err(CompileError::new(
                ErrorKind::PreviewFeatureRequired {
                    feature,
                    what: what.to_string(),
                },
                span,
            )
            .with_help(format!(
                "use `--preview {}` to enable this feature ({})",
                feature.name(),
                feature.adr()
            )))
        }
    }

    /// Check that we are inside a `checked` block.
    /// Returns an error if `checked_depth` is zero.
    fn require_checked_for_intrinsic(
        ctx: &AnalysisContext,
        intrinsic_name: &str,
        span: Span,
    ) -> CompileResult<()> {
        if ctx.checked_depth > 0 {
            Ok(())
        } else {
            Err(CompileError::new(
                ErrorKind::IntrinsicRequiresChecked(intrinsic_name.to_string()),
                span,
            ))
        }
    }

    fn analyze_single_function(
        &mut self,
        infer_ctx: &InferenceContext,
        fn_name: &str,
        return_type: Spur,
        params: &[gruel_rir::RirParam],
        body: InstRef,
        span: Span,
    ) -> AnalyzedFnResult {
        let ret_type = self.resolve_type(return_type, span)?;

        // Resolve parameter types and modes. Use `resolve_param_type` so
        // interface-typed parameters (ADR-0056 Phase 4) resolve correctly.
        let param_info: Vec<(Spur, Type, RirParamMode)> = params
            .iter()
            .map(|p| {
                let ty = self.resolve_param_type(p.ty, p.mode, span)?;
                Ok((p.name, ty, p.mode))
            })
            .collect::<CompileResult<Vec<_>>>()?;

        let (
            air,
            num_locals,
            num_param_slots,
            param_modes,
            param_slot_types,
            warnings,
            local_strings,
            local_bytes,
            ref_fns,
            ref_meths,
        ) = self.analyze_function(infer_ctx, ret_type, &param_info, body)?;

        Ok((
            AnalyzedFunction {
                name: fn_name.to_string(),
                air,
                num_locals,
                num_param_slots,
                param_modes,
                param_slot_types,
                is_destructor: false,
            },
            warnings,
            local_strings,
            local_bytes,
            ref_fns,
            ref_meths,
        ))
    }

    /// Analyze a method function from an impl block.
    ///
    /// The `infer_ctx` provides pre-computed type information for constraint generation.
    ///
    /// Returns the analyzed function, any warnings, and local strings collected during analysis.
    fn analyze_method_function(
        &mut self,
        infer_ctx: &InferenceContext,
        full_name: &str,
        spec: MethodBodySpec<'_>,
        span: Span,
    ) -> AnalyzedFnResult {
        let ret_type = self.resolve_type(spec.return_type, span)?;

        // Build parameter list, adding self as first parameter for methods
        let mut param_info: Vec<(Spur, Type, RirParamMode)> = Vec::new();

        if let Some(struct_type) = spec.self_type {
            // Decode the receiver mode set by the parser (`self` / `&self` /
            // `&mut self` — see ADR-0062). Receiver normalization parallels
            // the regular-parameter normalization in
            // `analyze_function_internal`: a `&self` (`SelfMode::Borrow`) self
            // param becomes `(Self, Borrow)` and `&mut self`
            // (`SelfMode::Inout`) becomes `(Self, Inout)`.
            let self_mode = match spec.self_mode {
                1 => RirParamMode::Inout,
                2 => RirParamMode::Borrow,
                _ => RirParamMode::Normal,
            };
            let self_sym = self.interner.get_or_intern("self");
            param_info.push((self_sym, struct_type, self_mode));
        }

        // Add regular parameters with their modes. Use `resolve_param_type`
        // for ADR-0056 interface-typed parameters.
        for p in spec.params.iter() {
            let ty = self.resolve_param_type(p.ty, p.mode, span)?;
            param_info.push((p.name, ty, p.mode));
        }

        let (
            air,
            num_locals,
            num_param_slots,
            param_modes,
            param_slot_types,
            warnings,
            local_strings,
            local_bytes,
            ref_fns,
            ref_meths,
        ) = self.analyze_function(infer_ctx, ret_type, &param_info, spec.body)?;

        Ok((
            AnalyzedFunction {
                name: full_name.to_string(),
                air,
                num_locals,
                num_param_slots,
                param_modes,
                param_slot_types,
                is_destructor: false,
            },
            warnings,
            local_strings,
            local_bytes,
            ref_fns,
            ref_meths,
        ))
    }

    /// Analyze a destructor function.
    ///
    /// The `infer_ctx` provides pre-computed type information for constraint generation.
    ///
    /// Returns the analyzed function, any warnings, and local strings collected during analysis.
    fn analyze_destructor_function(
        &mut self,
        infer_ctx: &InferenceContext,
        full_name: &str,
        body: InstRef,
        _span: Span,
        struct_type: Type,
    ) -> AnalyzedFnResult {
        // Destructors take self parameter and return unit
        let self_sym = self.interner.get_or_intern("self");
        let param_info: Vec<(Spur, Type, RirParamMode)> =
            vec![(self_sym, struct_type, RirParamMode::Normal)];

        let (
            air,
            num_locals,
            num_param_slots,
            param_modes,
            param_slot_types,
            warnings,
            local_strings,
            local_bytes,
            ref_fns,
            ref_meths,
        ) = self.analyze_function(infer_ctx, Type::UNIT, &param_info, body)?;

        Ok((
            AnalyzedFunction {
                name: full_name.to_string(),
                air,
                num_locals,
                num_param_slots,
                param_modes,
                param_slot_types,
                is_destructor: true,
            },
            warnings,
            local_strings,
            local_bytes,
            ref_fns,
            ref_meths,
        ))
    }
    /// Analyze a single function, producing AIR.
    ///
    /// The `infer_ctx` provides pre-computed type information for constraint generation,
    /// avoiding the cost of rebuilding maps for each function.
    ///
    /// Returns (air, num_locals, num_param_slots, param_modes, warnings).
    /// Warnings are collected per-function to enable future parallel analysis.
    fn analyze_function(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode)], // (name, type, mode)
        body: InstRef,
    ) -> RawFnAnalysis {
        self.analyze_function_internal(infer_ctx, return_type, params, body, None, None)
    }

    /// Internal function analysis with optional type substitutions.
    ///
    /// When `type_subst` is provided (for specialized generic functions), it populates
    /// `comptime_type_vars` so that type parameters can be resolved in struct initialization
    /// (e.g., `P { x: 1, y: 2 }` where `P` is a type parameter).
    fn analyze_function_internal(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode)],
        body: InstRef,
        type_subst: Option<&rustc_hash::FxHashMap<Spur, Type>>,
        value_subst: Option<&rustc_hash::FxHashMap<Spur, ConstValue>>,
    ) -> RawFnAnalysis {
        // ADR-0062: a parameter typed `Ref(T)` / `MutRef(T)` with the default
        // `Normal` mode is the new-form spelling of the legacy `borrow x: T` /
        // `inout x: T` keyword forms. Lower it to the legacy mode so the rest
        // of sema (place tracing, exclusivity, mutability, codegen) handles
        // both surface forms uniformly. Function bodies then see the bare `T`
        // and field projection / indexing / scalar reads / through-assignment
        // all work without a user-facing deref operator.
        let normalized_params: Vec<(Spur, Type, RirParamMode)> = params
            .iter()
            .map(|(name, ty, mode)| match (mode, ty.try_kind()) {
                (RirParamMode::Normal, Some(TypeKind::Ref(id))) => {
                    (*name, self.type_pool.ref_def(id), RirParamMode::Borrow)
                }
                (RirParamMode::Normal, Some(TypeKind::MutRef(id))) => {
                    (*name, self.type_pool.mut_ref_def(id), RirParamMode::Inout)
                }
                _ => (*name, *ty, *mode),
            })
            .collect();
        let params: &[(Spur, Type, RirParamMode)] = &normalized_params;

        let mut air = Air::new(return_type);
        let mut param_vec: Vec<ParamInfo> = Vec::new();
        let mut param_modes: Vec<crate::inst::AirParamMode> = Vec::new();
        let mut param_slot_types: Vec<Type> = Vec::new();

        // Add parameters to the param vec, tracking ABI slot offsets.
        // Each parameter starts at the next available ABI slot.
        // For struct parameters, the slot count is the number of fields.
        let mut next_abi_slot: u32 = 0;
        for (pname, ptype, mode) in params.iter() {
            param_vec.push(ParamInfo {
                name: *pname,
                abi_slot: next_abi_slot,
                ty: *ptype,
                mode: *mode,
            });
            // Inout and Borrow parameters are passed by reference.
            // Comptime parameters are VALUE params (like `comptime n: i32`), passed by value.
            // Normal parameters are passed by value.
            let air_mode: crate::inst::AirParamMode = (*mode).into();
            let is_by_ref = air_mode.is_by_ref();
            let slot_count = if is_by_ref {
                // By-ref parameters are always 1 slot (pointer)
                1
            } else {
                self.abi_slot_count(*ptype)
            };
            for _ in 0..slot_count {
                param_modes.push(air_mode);
                param_slot_types.push(*ptype);
            }
            next_abi_slot += slot_count;
        }
        let num_param_slots = next_abi_slot;

        // ======================================================================
        // Phase 1-2: Hindley-Milner Type Inference
        // ======================================================================
        // Run constraint generation and unification to determine types
        // for all expressions BEFORE emitting AIR.
        let resolved_types = self.run_type_inference(
            infer_ctx,
            return_type,
            params,
            body,
            type_subst,
            value_subst,
        )?;

        // Create analysis context with resolved types
        // If type_subst is provided, initialize comptime_type_vars with the substitutions
        // so that type parameters can be resolved during struct initialization.
        let comptime_type_vars = type_subst.cloned().unwrap_or_default();
        let comptime_value_vars = value_subst.cloned().unwrap_or_default();
        let mut ctx = AnalysisContext {
            locals: HashMap::default(),
            params: &param_vec,
            next_slot: 0,
            loop_depth: 0,
            forbid_break: None,
            checked_depth: 0,
            used_locals: HashSet::default(),
            return_type,
            scope_stack: Vec::new(),
            resolved_types: &resolved_types,
            moved_vars: HashMap::default(),
            warnings: Vec::new(),
            local_string_table: HashMap::default(),
            local_strings: Vec::new(),
            local_bytes: Vec::new(),
            comptime_type_vars,
            comptime_value_vars,
            referenced_functions: HashSet::default(),
            referenced_methods: HashSet::default(),
        };

        // ======================================================================
        // Phase 3: AIR Emission
        // ======================================================================
        // Analyze the body expression, emitting AIR with resolved types
        let body_result = self.analyze_inst(&mut air, body, &mut ctx)?;

        // Add implicit return only if body doesn't already diverge (e.g., explicit return)
        if body_result.ty != Type::NEVER {
            air.add_inst(AirInst {
                data: AirInstData::Ret(Some(body_result.air_ref)),
                ty: return_type,
                span: self.rir.get(body).span,
            });
        }

        Ok((
            air,
            ctx.next_slot,
            num_param_slots,
            param_modes,
            param_slot_types,
            ctx.warnings,
            ctx.local_strings,
            ctx.local_bytes,
            ctx.referenced_functions,
            ctx.referenced_methods,
        ))
    }

    /// Analyze a specialized function body.
    ///
    /// This is similar to `analyze_function` but for generic function specialization.
    /// The `type_subst` map provides substitutions for type parameters to their
    /// concrete types.
    ///
    /// For example, when specializing `fn identity<T>(x: T) -> T { x }` with `T = i32`,
    /// the `params` will be `[(x, i32, Normal)]` and `return_type` will be `i32`.
    pub fn analyze_specialized_function(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode)],
        body: InstRef,
        type_subst: &rustc_hash::FxHashMap<Spur, Type>,
    ) -> RawFnAnalysis {
        // For specialized functions, we need to populate comptime_type_vars with the
        // type substitutions so that references to type parameters (like `P { ... }`)
        // can be resolved in the function body.
        self.analyze_function_internal(infer_ctx, return_type, params, body, Some(type_subst), None)
    }

    /// Analyze a method body with `Self` type resolution.
    ///
    /// This is used for anonymous struct methods where `Self` should resolve to the
    /// struct type. The `self_type` is added to the type substitution map under the
    /// symbol "Self", allowing `Self { ... }` struct literals to work correctly.
    fn analyze_method_body(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode)],
        body: InstRef,
        self_type: Type,
        captured_comptime_values: &rustc_hash::FxHashMap<Spur, ConstValue>,
    ) -> RawFnAnalysis {
        // Create a type substitution map with Self -> the struct type
        let self_sym = self.interner.get_or_intern("Self");
        let mut type_subst = HashMap::default();
        type_subst.insert(self_sym, self_type);

        self.analyze_function_internal(
            infer_ctx,
            return_type,
            params,
            body,
            Some(&type_subst),
            Some(captured_comptime_values),
        )
    }

    /// Run Hindley-Milner type inference on a function body.
    ///
    /// This is Phases 1-2 of the HM algorithm:
    /// 1. Generate constraints by walking the RIR
    /// 2. Solve constraints via unification
    ///
    /// The `infer_ctx` parameter provides pre-computed type information (function
    /// signatures, struct/enum types, method signatures) converted to InferType format.
    /// This avoids rebuilding these maps for each function, reducing O(n²) to O(n).
    ///
    /// Returns a map from RIR instruction refs to their resolved concrete types.
    fn run_type_inference(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode)],
        body: InstRef,
        type_subst: Option<&HashMap<Spur, Type>>,
        value_subst: Option<&HashMap<Spur, ConstValue>>,
    ) -> CompileResult<HashMap<InstRef, Type>> {
        // Create constraint generator using pre-computed inference context
        let mut cgen =
            ConstraintGenerator::new(self.rir, self.interner, infer_ctx, &self.type_pool)
                .with_type_subst(type_subst);

        // Build parameter map for constraint context.
        // Convert Type to InferType so arrays are represented structurally.
        let mut param_vars: HashMap<Spur, ParamVarInfo> = params
            .iter()
            .map(|(name, ty, _mode)| {
                (
                    *name,
                    ParamVarInfo {
                        ty: self.type_to_infer_type(*ty),
                    },
                )
            })
            .collect();

        // Add comptime value variables as if they were parameters
        // This allows constraint generation to see captured comptime values
        if let Some(values) = value_subst {
            for (name, const_val) in values {
                let ty = match const_val {
                    ConstValue::Integer(_) => Type::COMPTIME_INT,
                    ConstValue::Bool(_) => Type::BOOL,
                    ConstValue::Type(t) => *t,
                    ConstValue::Unit => Type::UNIT,
                    ConstValue::ComptimeStr(_) => Type::COMPTIME_STR,
                    ConstValue::Struct(_)
                    | ConstValue::Array(_)
                    | ConstValue::EnumVariant { .. }
                    | ConstValue::EnumData { .. }
                    | ConstValue::EnumStruct { .. }
                    | ConstValue::BreakSignal
                    | ConstValue::ContinueSignal
                    | ConstValue::ReturnSignal => {
                        unreachable!(
                            "control-flow signal or composite value in comptime_value_vars"
                        )
                    }
                };
                param_vars.insert(
                    *name,
                    ParamVarInfo {
                        ty: self.type_to_infer_type(ty),
                    },
                );
            }
        }

        // Create constraint context
        let mut cgen_ctx = ConstraintContext::new(&param_vars, return_type);

        // Phase 1: Generate constraints
        let body_info = cgen.generate(body, &mut cgen_ctx);

        // The function body's type must match the return type.
        // This handles implicit returns like `fn foo() -> i8 { 42 }`.
        // For arrays, we need to convert Type to InferType structurally.
        cgen.add_constraint(Constraint::equal(
            body_info.ty,
            self.type_to_infer_type(return_type),
            body_info.span,
        ));

        // Consume the constraint generator to release borrows
        let (constraints, int_literal_vars, float_literal_vars, expr_types, type_var_count) =
            cgen.into_parts();

        // Phase 2: Solve constraints via unification
        // Pre-size the substitution for better performance on large functions
        let mut unifier = Unifier::with_capacity(type_var_count);
        let errors = unifier.solve_constraints(&constraints);

        // Convert unification errors to compile errors
        // For now, we collect the first error. In the future, we could
        // report multiple errors for better diagnostics.
        if let Some(err) = errors.first() {
            // Map each UnifyResult variant to the appropriate ErrorKind
            let error_kind = match &err.kind {
                UnifyResult::Ok => unreachable!("UnificationError should never contain Ok"),
                UnifyResult::TypeMismatch { expected, found } => ErrorKind::TypeMismatch {
                    expected: expected.to_string(),
                    found: found.to_string(),
                },
                UnifyResult::IntLiteralNonInteger { found } => ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: found.name().to_string(),
                },
                UnifyResult::OccursCheck { var, ty } => ErrorKind::TypeMismatch {
                    expected: "non-recursive type".to_string(),
                    found: format!("{var} = {ty} (infinite type)"),
                },
                UnifyResult::NotSigned { ty } => {
                    ErrorKind::CannotNegateUnsigned(ty.name().to_string())
                }
                UnifyResult::NotInteger { ty } => ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: ty.name().to_string(),
                },
                UnifyResult::NotUnsigned { ty } => ErrorKind::TypeMismatch {
                    expected: "unsigned integer type".to_string(),
                    found: ty.name().to_string(),
                },
                UnifyResult::NotNumeric { ty } => ErrorKind::TypeMismatch {
                    expected: "numeric type".to_string(),
                    found: ty.name().to_string(),
                },
                UnifyResult::ArrayLengthMismatch { expected, found } => {
                    ErrorKind::ArrayLengthMismatch {
                        expected: *expected,
                        found: *found,
                    }
                }
            };

            let mut compile_error = CompileError::new(error_kind, err.span);

            // Add note for unsigned negation errors
            if matches!(err.kind, UnifyResult::NotSigned { .. }) {
                compile_error = compile_error.with_note("unsigned values cannot be negated");
            }

            return Err(compile_error);
        }

        // Default any unconstrained integer literals to i32 and float literals to f64
        unifier.default_int_literal_vars(&int_literal_vars);
        unifier.default_float_literal_vars(&float_literal_vars);

        // Pre-collect all array types from resolved InferTypes before converting them.
        // This ensures all array types are created before the conversion loop, which
        // enables parallelization of function analysis (mutation happens here, not in
        // infer_type_to_type).
        for infer_ty in expr_types.values() {
            let resolved = unifier.resolve_infer_type(infer_ty);
            self.pre_create_array_types_from_infer_type(&resolved);
        }

        // Build the resolved types map, converting InferType to Type.
        // Since we pre-created all array types above, infer_type_to_type only
        // performs lookups (no mutation).
        let mut resolved_types = HashMap::default();
        for (inst_ref, infer_ty) in &expr_types {
            let resolved = unifier.resolve_infer_type(infer_ty);
            let concrete_ty = self.infer_type_to_type(&resolved);
            resolved_types.insert(*inst_ref, concrete_ty);
        }

        Ok(resolved_types)
    }
    /// Analyze an RIR instruction for projection (field access).
    ///
    /// This is like `analyze_inst` but does NOT mark non-Copy values as moved.
    /// Used for field access where we're reading from a struct without consuming it.
    /// We still check that the variable hasn't already been moved (fully moved).
    /// Field-level move checking is done at the FieldGet level, not here.
    pub(crate) fn analyze_inst_for_projection(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        // For VarRef, we handle it specially: check for full moves but don't mark as moved
        if let InstData::VarRef { name } = &inst.data {
            // First check if it's a parameter
            if let Some(param_info) = ctx.params.iter().find(|p| p.name == *name) {
                let ty = param_info.ty;

                // Check if this parameter has been fully moved
                // (Partial moves are checked at the FieldGet level)
                if let Some(move_state) = ctx.moved_vars.get(name)
                    && let Some(moved_span) = move_state.full_move
                {
                    let name_str = self.interner.resolve(name);
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(name_str.to_string()),
                        inst.span,
                    )
                    .with_label("value moved here", moved_span));
                }

                // NOTE: We do NOT mark as moved here - this is a projection

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Param {
                        index: param_info.abi_slot,
                    },
                    ty,
                    span: inst.span,
                });
                return Ok(AnalysisResult::new(air_ref, ty));
            }

            // Check if it's a local variable
            if let Some(local) = ctx.locals.get(name) {
                let ty = local.ty;
                let slot = local.slot;

                // Check if this variable has been fully moved
                // (Partial moves are checked at the FieldGet level)
                if let Some(move_state) = ctx.moved_vars.get(name)
                    && let Some(moved_span) = move_state.full_move
                {
                    let name_str = self.interner.resolve(name);
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(name_str.to_string()),
                        inst.span,
                    )
                    .with_label("value moved here", moved_span));
                }

                // NOTE: We do NOT mark as moved here - this is a projection

                // Mark variable as used
                ctx.used_locals.insert(*name);

                // Load the variable
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Load { slot },
                    ty,
                    span: inst.span,
                });
                return Ok(AnalysisResult::new(air_ref, ty));
            }

            // Check if it's a comptime type variable (e.g., `let P = Point();`)
            if let Some(&ty) = ctx.comptime_type_vars.get(name) {
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(ty),
                    ty: Type::COMPTIME_TYPE,
                    span: inst.span,
                });
                return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
            }

            // Check if it's a comptime value variable (e.g., captured `comptime N: i32`)
            if let Some(const_value) = ctx.comptime_value_vars.get(name) {
                match const_value {
                    ConstValue::Integer(val) => {
                        let ty = Self::get_resolved_type(
                            ctx,
                            inst_ref,
                            inst.span,
                            "comptime integer value",
                        )?;
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::Const(*val as u64),
                            ty,
                            span: inst.span,
                        });
                        return Ok(AnalysisResult::new(air_ref, ty));
                    }
                    ConstValue::Bool(val) => {
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::Const(*val as u64),
                            ty: Type::BOOL,
                            span: inst.span,
                        });
                        return Ok(AnalysisResult::new(air_ref, Type::BOOL));
                    }
                    ConstValue::Type(ty) => {
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::TypeConst(*ty),
                            ty: Type::COMPTIME_TYPE,
                            span: inst.span,
                        });
                        return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
                    }
                    ConstValue::ComptimeStr(_)
                    | ConstValue::Struct(_)
                    | ConstValue::Array(_)
                    | ConstValue::EnumVariant { .. }
                    | ConstValue::EnumData { .. }
                    | ConstValue::EnumStruct { .. } => {
                        return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: "comptime composite values cannot be used in runtime expressions; use @field to access fields".to_string(),
                            },
                            inst.span,
                        ));
                    }
                    ConstValue::Unit => {
                        return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason:
                                    "comptime unit values cannot be used in runtime expressions"
                                        .to_string(),
                            },
                            inst.span,
                        ));
                    }
                    ConstValue::BreakSignal
                    | ConstValue::ContinueSignal
                    | ConstValue::ReturnSignal => {
                        unreachable!("control-flow signal in comptime_value_vars")
                    }
                }
            }

            // Not found
            let name_str = self.interner.resolve(name);
            return Err(CompileError::new(
                ErrorKind::UndefinedVariable(name_str.to_string()),
                inst.span,
            ));
        }

        // For nested field access (e.g., a.b.c), recursively use projection mode
        if let InstData::FieldGet { base, field } = &inst.data {
            let base_result = self.analyze_inst_for_projection(air, *base, ctx)?;
            let base_type = base_result.ty;

            let struct_id = match base_type.kind() {
                TypeKind::Struct(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::FieldAccessOnNonStruct {
                            found: base_type.name().to_string(),
                        },
                        inst.span,
                    ));
                }
            };

            let struct_def = self.type_pool.struct_def(struct_id);
            let raw_field_name_str = self.interner.resolve(field).to_string();
            // Tuple-root match suffix marker `..end_N`: resolve to the
            // concrete tuple index now that we know the tuple's arity
            // (ADR-0049 Phase 6).
            let field_name_str = if let Some(rest) = raw_field_name_str.strip_prefix("..end_") {
                match rest.parse::<usize>() {
                    Ok(from_end) if from_end < struct_def.fields.len() => {
                        let idx = struct_def.fields.len() - 1 - from_end;
                        idx.to_string()
                    }
                    _ => raw_field_name_str.clone(),
                }
            } else {
                raw_field_name_str.clone()
            };

            let (field_index, struct_field) =
                struct_def.find_field(&field_name_str).ok_or_compile_error(
                    ErrorKind::UnknownField {
                        struct_name: struct_def.name.clone(),
                        field_name: field_name_str.clone(),
                    },
                    inst.span,
                )?;

            let field_type = struct_field.ty;

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::FieldGet {
                    base: base_result.air_ref,
                    struct_id,
                    field_index: field_index as u32,
                },
                ty: field_type,
                span: inst.span,
            });
            return Ok(AnalysisResult::new(air_ref, field_type));
        }

        // For index access in projection mode (e.g., `arr[i].field`), we allow the
        // indexing without checking if the element type is Copy. This enables
        // accessing Copy fields of non-Copy array elements.
        if let InstData::IndexGet { base, index } = &inst.data {
            // Recursively analyze the base in projection mode
            let base_result = self.analyze_inst_for_projection(air, *base, ctx)?;
            let base_type = base_result.ty;

            let array_type_id = match base_type.kind() {
                TypeKind::Array(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::IndexOnNonArray {
                            found: base_type.name().to_string(),
                        },
                        inst.span,
                    ));
                }
            };

            let (element_type, length) = self.type_pool.array_def(array_type_id);

            // Index must be `usize` (ADR-0054).
            let index_result = self.analyze_inst(air, *index, ctx)?;
            if index_result.ty != Type::USIZE && !index_result.ty.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "usize".to_string(),
                        found: index_result.ty.name().to_string(),
                    },
                    self.rir.get(*index).span,
                ));
            }

            let array_length = length;

            // Compile-time bounds check for constant indices
            if let Some(const_index) = self.try_get_const_index(*index)
                && (const_index < 0 || const_index as u64 >= array_length)
            {
                return Err(CompileError::new(
                    ErrorKind::IndexOutOfBounds {
                        index: const_index,
                        length: array_length,
                    },
                    self.rir.get(*index).span,
                ));
            }

            // NOTE: We do NOT check if element_type is Copy here.
            // In projection mode, we allow accessing elements for further projection
            // (e.g., arr[i].field where field is Copy).

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::IndexGet {
                    base: base_result.air_ref,
                    array_type: base_type,
                    index: index_result.air_ref,
                },
                ty: element_type,
                span: inst.span,
            });
            return Ok(AnalysisResult::new(air_ref, element_type));
        }

        // For other expressions, use the normal analyze_inst
        // (they will trigger move semantics as expected)
        self.analyze_inst(air, inst_ref, ctx)
    }

    /// Look up the resolved type for an instruction from HM inference.
    ///
    /// Returns an `InternalError` if the type was not resolved. This should
    /// never happen in normal operation, but provides a better error message
    /// than a panic if there's a bug in type inference.
    pub(crate) fn get_resolved_type(
        ctx: &AnalysisContext,
        inst_ref: InstRef,
        span: Span,
        context: &str,
    ) -> CompileResult<Type> {
        ctx.resolved_types.get(&inst_ref).copied().ok_or_else(|| {
            CompileError::new(
                ErrorKind::InternalError(format!(
                    "type inference did not resolve type for {} (instruction {:?})",
                    context, inst_ref
                )),
                span,
            )
        })
    }

    /// Analyze an RIR instruction, producing AIR instructions.
    ///
    /// Types are determined by Hindley-Milner inference (stored in `resolved_types`).
    /// Returns both the AIR reference and the synthesized type.
    /// Analyze a single RIR instruction and produce the corresponding AIR instruction.
    ///
    /// This method dispatches to category-specific methods in `analyze_ops.rs` for
    /// maintainability. Each category handles related instruction types together.
    ///
    /// # Categories
    ///
    /// - **Literals**: IntConst, BoolConst, StringConst, UnitConst
    /// - **Binary arithmetic**: Add, Sub, Mul, Div, Mod, BitAnd, BitOr, BitXor, Shl, Shr
    /// - **Comparison**: Eq, Ne, Lt, Gt, Le, Ge
    /// - **Logical**: And, Or
    /// - **Unary**: Neg, Not, BitNot
    /// - **Control flow**: Branch, Loop, InfiniteLoop, Match, Break, Continue, Ret, Block
    /// - **Variables**: Alloc, VarRef, ParamRef, Assign
    /// - **Structs**: StructDecl, StructInit, FieldGet, FieldSet
    /// - **Arrays**: ArrayInit, IndexGet, IndexSet
    /// - **Enums**: EnumDecl, EnumVariant
    /// - **Calls**: Call, MethodCall, AssocFnCall
    /// - **Intrinsics**: Intrinsic, TypeIntrinsic
    /// - **Declarations**: DropFnDecl, FnDecl
    pub(crate) fn analyze_inst(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            // Literals
            InstData::IntConst(_)
            | InstData::FloatConst(_)
            | InstData::BoolConst(_)
            | InstData::StringConst(_)
            | InstData::UnitConst => self.analyze_literal(air, inst_ref, ctx),

            InstData::Bin { op, lhs, rhs } => match op {
                BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Div
                | BinOp::Mod
                | BinOp::BitAnd
                | BinOp::BitOr
                | BinOp::BitXor
                | BinOp::Shl
                | BinOp::Shr => self.analyze_binary_arith(air, *lhs, *rhs, *op, inst.span, ctx),
                BinOp::Eq | BinOp::Ne => {
                    self.analyze_comparison(air, (*lhs, *rhs), true, *op, inst.span, ctx)
                }
                BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                    self.analyze_comparison(air, (*lhs, *rhs), false, *op, inst.span, ctx)
                }
                BinOp::And | BinOp::Or => self.analyze_logical_op(air, inst_ref, ctx),
            },

            InstData::Unary { .. } => self.analyze_unary_op(air, inst_ref, ctx),

            // Reference construction (ADR-0062): `&x` / `&mut x`.
            InstData::MakeRef { .. } => self.analyze_make_ref(air, inst_ref, ctx),

            // ADR-0064: slice construction by borrow over a range subscript
            // (`&arr[range]` / `&mut arr[range]`).
            InstData::MakeSlice { .. } => self.analyze_make_slice(air, inst_ref, ctx),

            // ADR-0064: range subscript without `&` / `&mut`.
            InstData::BareRangeSubscript => Err(CompileError::new(
                ErrorKind::ParseError(
                    "range subscripts produce slices and must be borrowed with `&` or `&mut`"
                        .to_string(),
                ),
                inst.span,
            )),

            // Control flow
            InstData::Branch { .. }
            | InstData::Loop { .. }
            | InstData::For { .. }
            | InstData::InfiniteLoop { .. }
            | InstData::Match { .. }
            | InstData::Break
            | InstData::Continue
            | InstData::Ret(_)
            | InstData::Block { .. } => self.analyze_control_flow(air, inst_ref, ctx),

            // Variable operations
            InstData::Alloc { .. }
            | InstData::StructDestructure { .. }
            | InstData::VarRef { .. }
            | InstData::ParamRef { .. }
            | InstData::Assign { .. } => self.analyze_variable_ops(air, inst_ref, ctx),

            // Struct operations
            InstData::StructDecl { .. }
            | InstData::StructInit { .. }
            | InstData::FieldGet { .. }
            | InstData::FieldSet { .. } => self.analyze_struct_ops(air, inst_ref, ctx),

            // Array operations
            InstData::ArrayInit { .. } | InstData::IndexGet { .. } | InstData::IndexSet { .. } => {
                self.analyze_array_ops(air, inst_ref, ctx)
            }

            // Enum operations
            InstData::EnumDecl { .. }
            | InstData::EnumVariant { .. }
            | InstData::EnumStructVariant { .. } => self.analyze_enum_ops(air, inst_ref, ctx),

            // Call operations
            InstData::Call { .. } | InstData::MethodCall { .. } | InstData::AssocFnCall { .. } => {
                self.analyze_call_ops(air, inst_ref, ctx)
            }

            // Intrinsic operations
            InstData::Intrinsic { .. }
            | InstData::TypeIntrinsic { .. }
            | InstData::TypeInterfaceIntrinsic { .. } => {
                self.analyze_intrinsic_ops(air, inst_ref, ctx)
            }

            // Declaration no-ops (produce Unit in expression context)
            InstData::DropFnDecl { .. }
            | InstData::FnDecl { .. }
            | InstData::ConstDecl { .. }
            | InstData::InterfaceDecl { .. }
            | InstData::InterfaceMethodSig { .. }
            | InstData::DeriveDecl { .. } => self.analyze_decl_noop(air, inst_ref, ctx),

            // Comptime block expression
            InstData::Comptime { expr } => {
                let span = inst.span;
                let expr = *expr;
                // Use the stateful comptime interpreter (Phase 1a).
                // This supports mutable let bindings, if/else, and blocks
                // in addition to pure arithmetic expressions.
                match self.evaluate_comptime_block(expr, ctx, span)? {
                    ConstValue::Integer(value) => {
                        let ty =
                            Self::get_resolved_type(ctx, inst_ref, span, "comptime block")?;
                        if value < 0 {
                            return Err(CompileError::new(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: "negative values not yet supported in comptime"
                                        .to_string(),
                                },
                                span,
                            ));
                        }
                        let unsigned_value = value as u64;
                        if !ty.literal_fits(unsigned_value) {
                            return Err(CompileError::new(
                                ErrorKind::LiteralOutOfRange {
                                    value: unsigned_value,
                                    ty: ty.name().to_string(),
                                },
                                span,
                            ));
                        }
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::Const(unsigned_value),
                            ty,
                            span,
                        });
                        Ok(AnalysisResult::new(air_ref, ty))
                    }
                    ConstValue::Bool(value) => {
                        let ty = Type::BOOL;
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::BoolConst(value),
                            ty,
                            span,
                        });
                        Ok(AnalysisResult::new(air_ref, ty))
                    }
                    ConstValue::Type(_) => Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: "type values cannot exist at runtime".to_string(),
                        },
                        span,
                    )),
                    ConstValue::ComptimeStr(str_idx) => {
                        // Materialize comptime string as a runtime String constant.
                        let content =
                            self.resolve_comptime_str(str_idx, span)?.to_string();
                        let ty = self.builtin_string_type();
                        let local_string_id = ctx.add_local_string(content);
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::StringConst(local_string_id),
                            ty,
                            span,
                        });
                        Ok(AnalysisResult::new(air_ref, ty))
                    }
                    ConstValue::Unit => {
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::UnitConst,
                            ty: Type::UNIT,
                            span,
                        });
                        Ok(AnalysisResult::new(air_ref, Type::UNIT))
                    }
                    // Composite comptime values (structs, arrays, enums) cannot be placed at
                    // runtime directly. The user must access individual fields/elements.
                    ConstValue::Struct(_)
                    | ConstValue::Array(_)
                    | ConstValue::EnumVariant { .. }
                    | ConstValue::EnumData { .. }
                    | ConstValue::EnumStruct { .. } => {
                        Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: "comptime composite values cannot be used at runtime; access individual fields or elements instead".into(),
                            },
                            span,
                        ))
                    }
                    // These signals are consumed by loop/call handlers inside evaluate_comptime_block.
                    // If they escape here, it means break/continue outside a loop, or return outside
                    // a function, which evaluate_comptime_block converts to an error before returning.
                    ConstValue::BreakSignal
                    | ConstValue::ContinueSignal
                    | ConstValue::ReturnSignal => {
                        unreachable!("control-flow signal escaped evaluate_comptime_block")
                    }
                }
            }

            // Comptime unroll for: evaluate iterable at comptime, unroll body N times
            InstData::ComptimeUnrollFor {
                binding,
                iterable,
                body,
            } => {
                let span = inst.span;
                let binding = *binding;
                let iterable = *iterable;
                let body = *body;

                // Step 1: Evaluate the iterable expression at comptime.
                // We use evaluate_comptime_block which clears and rebuilds the heap.
                let iterable_val = self.evaluate_comptime_block(iterable, ctx, span)?;

                // Step 2: Extract array elements from the comptime heap.
                // We clone the elements AND preserve the heap so that composite
                // ConstValues (e.g., Struct(heap_idx)) remain valid during iteration.
                let elements = match iterable_val {
                    ConstValue::Array(heap_idx) => match &self.comptime_heap[heap_idx as usize] {
                        ComptimeHeapItem::Array(elems) => elems.clone(),
                        _ => {
                            return Err(CompileError::new(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: "comptime_unroll iterable is not an array".to_string(),
                                },
                                span,
                            ));
                        }
                    },
                    _ => {
                        return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: "comptime_unroll for requires an array iterable"
                                    .to_string(),
                            },
                            span,
                        ));
                    }
                };

                // Step 3: For each element, bind the loop variable and analyze the body.
                // The loop variable is stored in comptime_value_vars so that @field
                // and other comptime expressions in the body can access it.
                let mut body_air_refs = Vec::with_capacity(elements.len());
                for element in &elements {
                    // Insert the loop variable as a comptime value
                    let prev_value = ctx.comptime_value_vars.insert(binding, *element);

                    // Analyze the body block
                    let body_result = self.analyze_inst(air, body, ctx)?;
                    body_air_refs.push(body_result.air_ref);

                    // Restore the previous value (or remove if there was none)
                    match prev_value {
                        Some(v) => {
                            ctx.comptime_value_vars.insert(binding, v);
                        }
                        None => {
                            ctx.comptime_value_vars.remove(&binding);
                        }
                    }
                }

                // Step 4: Emit all unrolled body instructions.
                if body_air_refs.is_empty() {
                    // Empty loop — emit unit constant
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::UnitConst,
                        ty: Type::UNIT,
                        span,
                    });
                    Ok(AnalysisResult::new(air_ref, Type::UNIT))
                } else if body_air_refs.len() == 1 {
                    Ok(AnalysisResult::new(body_air_refs[0], Type::UNIT))
                } else {
                    // Emit a block containing all unrolled body results.
                    // The last body is the block's "value"; the rest are statements.
                    let last = body_air_refs.pop().unwrap();
                    let stmts: Vec<u32> = body_air_refs.iter().map(|r| r.as_u32()).collect();
                    let stmts_start = air.add_extra(&stmts);
                    let block_ref = air.add_inst(AirInst {
                        data: AirInstData::Block {
                            stmts_start,
                            stmts_len: stmts.len() as u32,
                            value: last,
                        },
                        ty: Type::UNIT,
                        span,
                    });
                    Ok(AnalysisResult::new(block_ref, Type::UNIT))
                }
            }

            // Type constant: a type used as a value (e.g., `i32` in `identity(i32, 42)`)
            InstData::TypeConst { type_name } => {
                // Resolve the type name to a concrete type
                let ty = self.resolve_type(*type_name, inst.span)?;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(ty),
                    ty: Type::COMPTIME_TYPE,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE))
            }

            // Anonymous struct type: a struct type constructed at comptime
            // (e.g., `struct { first: T, second: T, fn get(self) -> T { ... } }` in a comptime function)
            InstData::AnonStructType {
                fields_start,
                fields_len,
                methods_start,
                methods_len,
                ..
            } => {
                // Get the field declarations from the RIR
                let field_decls = self.rir.get_field_decls(*fields_start, *fields_len);

                // Empty structs are not allowed (unless they have methods)
                if field_decls.is_empty() && *methods_len == 0 {
                    return Err(CompileError::new(ErrorKind::EmptyStruct, inst.span));
                }

                // Methods are fully supported (anon_struct_methods stabilized)

                // Resolve each field type and build the struct fields
                let mut struct_fields = Vec::with_capacity(field_decls.len());
                for (name_sym, type_sym) in field_decls {
                    let name_str = self.interner.resolve(&name_sym).to_string();
                    let field_ty = self.resolve_type(type_sym, inst.span)?;
                    struct_fields.push(StructField {
                        name: name_str,
                        ty: field_ty,
                    });
                }

                // Extract method signatures for structural equality comparison
                // (uses type symbols, not resolved Types, so Self matches Self)
                let method_sigs = self.extract_anon_method_sigs(*methods_start, *methods_len);

                // Check if an equivalent anonymous struct already exists (structural equality)
                // This now compares fields, method signatures, AND captured comptime values
                let (struct_ty, _is_new) = self.find_or_create_anon_struct(
                    &struct_fields,
                    &method_sigs,
                    &HashMap::default(),
                );

                // DON'T register methods here - they should be registered during const evaluation
                // (either try_evaluate_const for non-comptime, or try_evaluate_const_with_subst for comptime).
                // If we register here, we create a struct without captured comptime values, which is incorrect.
                //
                // if is_new && *methods_len > 0 {
                //     let struct_id = struct_ty
                //         .as_struct()
                //         .expect("anon struct should have StructId");
                //     self.register_anon_struct_methods(
                //         struct_id,
                //         struct_ty,
                //         *methods_start,
                //         *methods_len,
                //         inst.span,
                //     )?;
                // }

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(struct_ty),
                    ty: Type::COMPTIME_TYPE,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE))
            }

            // Anonymous interface type (ADR-0057): an interface type
            // constructed at comptime, e.g.
            // `interface { fn size(self) -> T }` inside a `fn ... -> type`
            // body. Resolves the method-signature types under the current
            // substitution map (none here — see the comptime evaluator path
            // for substituted resolution), then either dedupes against an
            // existing structurally-equal interface or registers a new
            // `InterfaceDef` and returns its id as a `Type::COMPTIME_TYPE`
            // value.
            InstData::AnonInterfaceType {
                methods_start,
                methods_len,
            } => {
                let req = self.build_anon_interface_def(
                    *methods_start,
                    *methods_len,
                    inst.span,
                    &rustc_hash::FxHashMap::default(),
                )?;
                let iface_id = self.find_or_create_anon_interface(req);
                let iface_ty = Type::new_interface(iface_id);
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(iface_ty),
                    ty: Type::COMPTIME_TYPE,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE))
            }

            // Anonymous enum type: an enum type constructed at comptime
            // (e.g., `enum { Some(T), None, fn method(self) -> bool { ... } }` in a comptime function)
            InstData::AnonEnumType {
                variants_start,
                variants_len,
                methods_start,
                methods_len,
                ..
            } => {
                // Get the variant declarations from the RIR
                let variant_decls = self
                    .rir
                    .get_enum_variant_decls(*variants_start, *variants_len);

                // Empty enums are not allowed
                if variant_decls.is_empty() {
                    return Err(CompileError::new(ErrorKind::EmptyAnonEnum, inst.span));
                }

                // Resolve each variant and build the enum variants
                let mut enum_variants = Vec::with_capacity(variant_decls.len());
                for (name_sym, field_type_syms, field_name_syms) in &variant_decls {
                    let name_str = self.interner.resolve(name_sym).to_string();
                    let mut fields = Vec::with_capacity(field_type_syms.len());
                    for ty_sym in field_type_syms {
                        let field_ty = self.resolve_type(*ty_sym, inst.span)?;
                        fields.push(field_ty);
                    }
                    let field_names: Vec<String> = field_name_syms
                        .iter()
                        .map(|s| self.interner.resolve(s).to_string())
                        .collect();
                    enum_variants.push(EnumVariantDef {
                        name: name_str,
                        fields,
                        field_names,
                    });
                }

                // Check for duplicate method names
                if *methods_len > 0 {
                    let method_refs = self.rir.get_inst_refs(*methods_start, *methods_len);
                    let mut seen_method_names: rustc_hash::FxHashSet<Spur> =
                        rustc_hash::FxHashSet::default();
                    for mref in method_refs {
                        let minst = self.rir.get(mref);
                        if let InstData::FnDecl {
                            name: method_name, ..
                        } = &minst.data
                            && !seen_method_names.insert(*method_name)
                        {
                            let method_name_str = self.interner.resolve(method_name).to_string();
                            return Err(CompileError::new(
                                ErrorKind::DuplicateMethod {
                                    type_name: "anonymous enum".to_string(),
                                    method_name: method_name_str,
                                },
                                minst.span,
                            ));
                        }
                    }
                }

                // Extract method signatures for structural equality comparison
                let method_sigs = self.extract_anon_method_sigs(*methods_start, *methods_len);

                // Check if an equivalent anonymous enum already exists (structural equality)
                let (enum_ty, _is_new) = self.find_or_create_anon_enum(
                    &enum_variants,
                    &method_sigs,
                    &HashMap::default(),
                );

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(enum_ty),
                    ty: Type::COMPTIME_TYPE,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE))
            }

            // Checked block: enter checked context, evaluate inner expression, then exit
            InstData::Checked { expr } => {
                ctx.checked_depth += 1;
                let result = self.analyze_inst(air, *expr, ctx);
                ctx.checked_depth -= 1;
                result
            }

            // Tuple literal (ADR-0048): lower to an anon struct with fields "0", "1", ...
            InstData::TupleInit {
                elems_start,
                elems_len,
            } => self.analyze_tuple_init(air, *elems_start, *elems_len, inst.span, ctx),

            // Anonymous function value (ADR-0055): synthesize a fresh anon
            // struct with zero fields and one `__call` method, then emit an
            // empty StructInit against it. Phase 2 uses the normal structural-
            // dedup path; Phase 3 makes each lambda site unique.
            InstData::AnonFnValue { method } => {
                self.analyze_anon_fn_value(air, *method, inst.span, ctx)
            }
        }
    }

    // ========================================================================
    // Implementation methods for complex operations
    // These are called by the category methods in analyze_ops.rs
    // ========================================================================

    /// Implementation for FieldSet - handles both local and parameter field assignment.
    pub(crate) fn analyze_field_set_impl(
        &mut self,
        air: &mut Air,
        base: InstRef,
        field: Spur,
        value: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        use crate::sema::analyze_ops::ProjectionInfo;

        // Try to trace the base to a place
        if let Some(mut trace) = self.try_trace_place(base, air, ctx)? {
            // Check if the root variable was fully moved
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

            // Check mutability
            let root_name = self.interner.resolve(&trace.root_var).to_string();
            if !trace.is_root_mutable {
                // Check if this is a borrow parameter - special error message
                if trace.is_borrow_param {
                    return Err(CompileError::new(
                        ErrorKind::MutateBorrowedValue {
                            variable: root_name,
                        },
                        span,
                    ));
                }

                let root_type = trace.base_type;
                // Provide more specific error based on whether it's a param or local
                match trace.base {
                    AirPlaceBase::Param(_) => {
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(root_name.clone()),
                            span,
                        )
                        .with_help(format!(
                            "consider making parameter `{}` inout: `inout {}: {}`",
                            root_name,
                            root_name,
                            root_type.name()
                        )));
                    }
                    AirPlaceBase::Local(_) => {
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(root_name),
                            span,
                        ));
                    }
                }
            }

            // Add the final field projection
            let base_type = trace.result_type();
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

            // Add the field projection to the trace
            trace.projections.push(ProjectionInfo {
                proj: AirProjection::Field {
                    struct_id,
                    field_index: field_index as u32,
                },
                result_type: field_type,
                field_name: Some(field),
            });

            // Analyze the value
            let value_result = self.analyze_inst(air, value, ctx)?;

            // Emit PlaceWrite instruction
            let place_ref = Self::build_place_ref(air, &trace);
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::PlaceWrite {
                    place: place_ref,
                    value: value_result.air_ref,
                },
                ty: Type::UNIT,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::UNIT));
        }

        // Fallback: base is not a place (e.g., function call result)
        // This shouldn't normally happen for valid assignment targets
        Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span))
    }

    /// Implementation for IndexSet - handles both local and parameter array index assignment.
    pub(crate) fn analyze_index_set_impl(
        &mut self,
        air: &mut Air,
        base: InstRef,
        index: InstRef,
        value: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        use crate::sema::analyze_ops::ProjectionInfo;

        // Try to trace the base to a place
        if let Some(mut trace) = self.try_trace_place(base, air, ctx)? {
            // Check if the root variable was fully moved
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

            // Check mutability
            let root_name = self.interner.resolve(&trace.root_var).to_string();
            if !trace.is_root_mutable {
                // Check if this is a borrow parameter - special error message
                if trace.is_borrow_param {
                    return Err(CompileError::new(
                        ErrorKind::MutateBorrowedValue {
                            variable: root_name,
                        },
                        span,
                    ));
                }

                let root_type = trace.base_type;
                match trace.base {
                    AirPlaceBase::Param(_) => {
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(root_name.clone()),
                            span,
                        )
                        .with_help(format!(
                            "consider making parameter `{}` inout: `inout {}: {}`",
                            root_name,
                            root_name,
                            root_type.name()
                        )));
                    }
                    AirPlaceBase::Local(_) => {
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(root_name),
                            span,
                        ));
                    }
                }
            }

            // Get array type info from the trace
            let base_type = trace.result_type();
            let (_array_type_id, elem_type, array_len) = match base_type.as_array() {
                Some(id) => {
                    let (elem, len) = self.type_pool.array_def(id);
                    (id, elem, len)
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

            // Analyze index. Must be `usize` (ADR-0054).
            let index_result = self.analyze_inst(air, index, ctx)?;
            if index_result.ty != Type::USIZE && !index_result.ty.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "usize".to_string(),
                        found: index_result.ty.name().to_string(),
                    },
                    self.rir.get(index).span,
                ));
            }

            // Compile-time bounds check for constant indices
            if let Some(const_index) = self.try_get_const_index(index)
                && (const_index < 0 || const_index as u64 >= array_len)
            {
                return Err(CompileError::new(
                    ErrorKind::IndexOutOfBounds {
                        index: const_index,
                        length: array_len,
                    },
                    self.rir.get(index).span,
                ));
            }

            // Add the index projection
            trace.projections.push(ProjectionInfo {
                proj: AirProjection::Index {
                    array_type: base_type,
                    index: index_result.air_ref,
                },
                result_type: elem_type,
                field_name: None,
            });

            // Analyze the value
            let value_result = self.analyze_inst(air, value, ctx)?;

            // Emit PlaceWrite instruction
            let place_ref = Self::build_place_ref(air, &trace);
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::PlaceWrite {
                    place: place_ref,
                    value: value_result.air_ref,
                },
                ty: Type::UNIT,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::UNIT));
        }

        // Fallback: base is not a place
        Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span))
    }

    /// Implementation for MethodCall.
    pub(crate) fn analyze_method_call_impl(
        &mut self,
        air: &mut Air,
        receiver: InstRef,
        method: Spur,
        args: Vec<RirCallArg>,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let receiver_var = self.extract_root_variable(receiver);
        let method_name_str = self.interner.resolve(&method).to_string();

        // Check if this is a builtin mutation method
        let is_builtin_mutation_method = self.is_builtin_mutation_method(&method_name_str);

        // Get storage location for mutation methods before analyzing receiver
        let receiver_storage = if is_builtin_mutation_method {
            self.get_string_receiver_storage(receiver, ctx, span)?
        } else {
            None
        };

        // Analyze the receiver expression
        let receiver_result = self.analyze_inst(air, receiver, ctx)?;
        let receiver_type = receiver_result.ty;

        // Handle module member access: module.function() becomes a direct function call
        if receiver_type.is_module() {
            return self.analyze_module_member_call_impl(air, method, args, span, ctx);
        }

        // Check that receiver is a struct or enum type
        // For enum methods, dispatch through enum_methods table
        if let TypeKind::Enum(enum_id) = receiver_type.kind() {
            let enum_def = self.type_pool.enum_def(enum_id);
            let enum_name_str = enum_def.name.clone();

            let method_key = (enum_id, method);
            let method_info = self.enum_methods.get(&method_key).ok_or_compile_error(
                ErrorKind::UndefinedMethod {
                    type_name: enum_name_str.clone(),
                    method_name: method_name_str.clone(),
                },
                span,
            )?;

            if !method_info.has_self {
                return Err(CompileError::new(
                    ErrorKind::AssocFnCalledAsMethod {
                        type_name: enum_name_str,
                        function_name: method_name_str,
                    },
                    span,
                ));
            }

            let method_param_types = self.param_arena.types(method_info.params);
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

            // ADR-0062: undo the receiver move and pass it by-pointer when the
            // method takes `&self` / `&mut self`. Mirrors the struct-method
            // path below.
            let recv_pass_mode = match method_info.receiver {
                crate::types::ReceiverMode::ByValue => AirArgMode::Normal,
                crate::types::ReceiverMode::Borrow => AirArgMode::Borrow,
                crate::types::ReceiverMode::Inout => AirArgMode::Inout,
            };
            if !matches!(method_info.receiver, crate::types::ReceiverMode::ByValue)
                && let Some(var) = receiver_var
            {
                ctx.moved_vars.remove(&var);
            }

            let return_type = method_info.return_type;

            let mut air_args = vec![AirCallArg {
                value: receiver_result.air_ref,
                mode: recv_pass_mode,
            }];
            air_args.extend(self.analyze_call_args(air, &args, ctx)?);

            let call_name = format!("{}.{}", enum_name_str, method_name_str);
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
            return Ok(AnalysisResult::new(air_ref, return_type));
        }

        // ADR-0056 Phase 4d-extended: dispatch method calls on interface
        // receivers dynamically via the vtable. The body type-checks against
        // the interface's declared signature (not against any concrete
        // implementor).
        if let TypeKind::Interface(iface_id) = receiver_type.kind() {
            // Interface dispatch passes the data pointer to the dispatched
            // method without copying or moving the underlying value, so the
            // receiver behaves like a borrow at the move-checker level.
            // Undo any move that `analyze_inst` may have recorded for the
            // root variable.
            if let Some(var) = receiver_var {
                ctx.moved_vars.remove(&var);
            }
            let iface_def = self.interface_defs[iface_id.0 as usize].clone();
            let (slot, req) = iface_def
                .find_method(&method_name_str)
                .map(|(s, r)| (s, r.clone()))
                .ok_or_compile_error(
                    ErrorKind::UndefinedMethod {
                        type_name: format!("interface `{}`", iface_def.name),
                        method_name: method_name_str.clone(),
                    },
                    span,
                )?;

            // Argument count check.
            if args.len() != req.param_types.len() {
                return Err(CompileError::new(
                    ErrorKind::WrongArgumentCount {
                        expected: req.param_types.len(),
                        found: args.len(),
                    },
                    span,
                ));
            }

            self.check_exclusive_access(&args, span)?;

            let air_args = self.analyze_call_args(air, &args, ctx)?;

            // Type-check each arg against the interface's declared param type.
            // `Self` slots (ADR-0060) are substituted with the interface type
            // itself — at a dynamic dispatch site there is no concrete
            // candidate to bind to, so `Self` flows through as the receiver's
            // static type.
            let iface_ty = receiver_type;
            for (i, (arg_air, req_ty)) in air_args.iter().zip(req.param_types.iter()).enumerate() {
                let expected_ty = req_ty.substitute_self(iface_ty);
                let actual_ty = air.get(arg_air.value).ty;
                if actual_ty != expected_ty {
                    let arg_span = self.rir.get(args[i].value).span;
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: expected_ty.name().to_string(),
                            found: actual_ty.name().to_string(),
                        },
                        arg_span,
                    ));
                }
            }

            // Encode args (excluding the receiver) into the extra array.
            let mut extra_data = Vec::with_capacity(air_args.len() * 2);
            for arg in &air_args {
                extra_data.push(arg.value.as_u32());
                extra_data.push(arg.mode.as_u32());
            }
            let dyn_args_start = air.add_extra(&extra_data);
            let dyn_args_len = air_args.len() as u32;

            let return_type = req.return_type.substitute_self(iface_ty);
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::MethodCallDyn {
                    interface_id: iface_id,
                    slot: slot as u32,
                    recv: receiver_result.air_ref,
                    args_start: dyn_args_start,
                    args_len: dyn_args_len,
                },
                ty: return_type,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, return_type));
        }

        // ADR-0063: methods on `Ptr(T)` / `MutPtr(T)` values dispatch through
        // the POINTER_METHODS registry to existing pointer intrinsics.
        if matches!(
            receiver_type.kind(),
            TypeKind::PtrConst(_) | TypeKind::PtrMut(_)
        ) {
            return self.dispatch_pointer_method_call(
                air,
                receiver_result,
                &method_name_str,
                &args,
                span,
                ctx,
            );
        }

        // ADR-0064: methods on `Slice(T)` / `MutSlice(T)` values dispatch
        // through the SLICE_METHODS registry.
        if matches!(
            receiver_type.kind(),
            TypeKind::Slice(_) | TypeKind::MutSlice(_)
        ) {
            return self.dispatch_slice_method_call(
                air,
                receiver_result,
                &method_name_str,
                &args,
                span,
                ctx,
            );
        }

        let struct_id = match receiver_type.kind() {
            TypeKind::Struct(id) => id,
            _ => {
                return Err(CompileError::new(
                    ErrorKind::MethodCallOnNonStruct {
                        found: receiver_type.name().to_string(),
                        method_name: method_name_str,
                    },
                    span,
                ));
            }
        };

        // Check if this is a builtin type and handle its methods
        if let Some(builtin_def) = self.get_builtin_type_def(struct_id) {
            let method_ctx = BuiltinMethodContext {
                struct_id,
                builtin_def,
                method_name: &method_name_str,
                span,
            };
            let receiver_info = ReceiverInfo {
                result: receiver_result,
                var: receiver_var,
                storage: receiver_storage,
            };
            return self.analyze_builtin_method(air, ctx, &method_ctx, receiver_info, &args);
        }

        // Look up the struct name by its ID (for error messages)
        let struct_def = self.type_pool.struct_def(struct_id);
        let struct_name_str = struct_def.name.clone();

        // Look up the method using StructId directly. Copy out so the
        // borrow on `self.methods` doesn't conflict with later mutable
        // borrows of `self` (e.g. `analyze_call_args`).
        let method_key = (struct_id, method);
        let method_info: MethodInfo = *self.methods.get(&method_key).ok_or_compile_error(
            ErrorKind::UndefinedMethod {
                type_name: struct_name_str.clone(),
                method_name: method_name_str.clone(),
            },
            span,
        )?;
        let method_info = &method_info;

        // Check that this is a method (has self), not an associated function
        if !method_info.has_self {
            return Err(CompileError::new(
                ErrorKind::AssocFnCalledAsMethod {
                    type_name: struct_name_str,
                    function_name: method_name_str,
                },
                span,
            ));
        }

        // ADR-0062: a `&self` / `&mut self` receiver is sugar for a borrow
        // (immutable / mutable). The receiver expression's `analyze_inst`
        // already recorded a move on the root variable since it was
        // evaluated as a value; undo it so the caller can keep using the
        // value after the call. This mirrors the interface-dispatch and
        // builtin-method paths above.
        let recv_pass_mode = match method_info.receiver {
            crate::types::ReceiverMode::ByValue => AirArgMode::Normal,
            crate::types::ReceiverMode::Borrow => AirArgMode::Borrow,
            crate::types::ReceiverMode::Inout => AirArgMode::Inout,
        };
        if !matches!(method_info.receiver, crate::types::ReceiverMode::ByValue)
            && let Some(var) = receiver_var
        {
            ctx.moved_vars.remove(&var);
        }

        // Check if calling an unchecked method requires a checked block
        if method_info.is_unchecked && ctx.checked_depth == 0 {
            return Err(CompileError::new(
                ErrorKind::UncheckedCallRequiresChecked(format!(
                    "{}.{}",
                    struct_name_str, method_name_str
                )),
                span,
            ));
        }

        // Clone data needed before mutable borrow
        let is_method_generic = method_info.is_generic;
        let method_param_comptime = self.param_arena.comptime(method_info.params).to_vec();
        let method_param_names = self.param_arena.names(method_info.params).to_vec();
        let return_type_for_call = method_info.return_type;
        let method_return_type_sym = method_info.return_type_sym;
        let method_param_types = self.param_arena.types(method_info.params).to_vec();

        // Argument count check is split between generic and non-generic methods.
        // Generic methods (ADR-0055) accept either the full arg list (explicit
        // mode) or just the runtime args (inference mode), so we skip the check
        // here for generics and validate later inside the inference branch.
        if !is_method_generic && args.len() != method_param_types.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: method_param_types.len(),
                    found: args.len(),
                },
                span,
            ));
        }

        // Check for exclusive access violation
        self.check_exclusive_access(&args, span)?;

        // ADR-0055 / method-level generics: if this method has comptime type
        // params (e.g. `comptime F: type`), emit a CallGeneric instruction
        // with the type arguments — either extracted from the call args
        // (explicit) or inferred from the runtime arg types (when the user
        // omits the comptime type args entirely). The specialization pass
        // synthesizes a specialized method body using these type args.
        if is_method_generic {
            let total_params = method_param_types.len();
            let runtime_param_count = method_param_comptime.iter().filter(|c| !**c).count();

            // Two valid call shapes:
            //   1. Explicit: every comptime + runtime param has an arg.
            //   2. Inferred: only runtime params have args; comptime types
            //      are recovered from the runtime arg types.
            // Anything else is a count mismatch.
            let infer_comptimes = if args.len() == total_params {
                false
            } else if args.len() == runtime_param_count && runtime_param_count < total_params {
                true
            } else {
                return Err(CompileError::new(
                    ErrorKind::WrongArgumentCount {
                        expected: total_params,
                        found: args.len(),
                    },
                    span,
                ));
            };

            // Phase 1: extract or analyze, in two paths that converge on
            // (type_args, type_subst, air_runtime_args).
            let mut type_args: Vec<Type> = Vec::new();
            let mut type_subst: rustc_hash::FxHashMap<Spur, Type> =
                rustc_hash::FxHashMap::default();
            let air_runtime_args: Vec<AirCallArg>;

            if !infer_comptimes {
                // Explicit mode: walk args in order, picking out comptime
                // type args and analyzing runtime args.
                let mut runtime_args: Vec<RirCallArg> = Vec::new();
                for (idx, arg) in args.iter().enumerate() {
                    if method_param_comptime[idx] {
                        let ty = self.resolve_method_generic_type_arg(
                            arg.value,
                            method_param_names[idx],
                            ctx,
                        )?;
                        type_args.push(ty);
                        type_subst.insert(method_param_names[idx], ty);
                    } else {
                        runtime_args.push(arg.clone());
                    }
                }
                air_runtime_args = self.analyze_call_args(air, &runtime_args, ctx)?;
            } else {
                // Inference mode: analyze all user-supplied args (they are
                // all runtime), then recover each comptime type param by
                // structural unification against a later runtime param's
                // declared type.
                air_runtime_args = self.analyze_call_args(air, &args, ctx)?;

                let runtime_arg_tys: Vec<Type> = air_runtime_args
                    .iter()
                    .map(|a| air.get(a.value).ty)
                    .collect();

                // Look up the declared type symbols of the original method
                // params from RIR (the resolved types are placeholders).
                let param_decl_tys =
                    self.method_param_type_syms(method_info.body)
                        .ok_or_else(|| {
                            CompileError::new(
                                ErrorKind::InternalError(
                                    "generic method has no FnDecl in RIR".to_string(),
                                ),
                                span,
                            )
                        })?;

                for (i, is_comptime) in method_param_comptime.iter().enumerate() {
                    if !is_comptime {
                        continue;
                    }
                    let cp_name = method_param_names[i];
                    // Find a runtime param at full position j > i whose
                    // declared type symbol references this comptime type
                    // param (bare match `j: T` for now).
                    let mut runtime_pos = 0usize;
                    let mut inferred: Option<Type> = None;
                    for (j, j_is_comptime) in method_param_comptime.iter().enumerate() {
                        if *j_is_comptime {
                            continue;
                        }
                        if j > i && param_decl_tys[j] == cp_name {
                            inferred = Some(runtime_arg_tys[runtime_pos]);
                            break;
                        }
                        runtime_pos += 1;
                    }
                    let inferred_ty = inferred.ok_or_else(|| {
                        CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: format!(
                                    "cannot infer comptime type parameter `{}`; \
                                     pass it explicitly",
                                    self.interner.resolve(&cp_name)
                                ),
                            },
                            span,
                        )
                    })?;
                    type_args.push(inferred_ty);
                    type_subst.insert(cp_name, inferred_ty);
                }
            }

            // Substitute the return type if it references any method-level
            // type params. Handles the simple case `-> U` (look up in the
            // substitution map) as well as compound cases like `-> [U; N]`
            // and `-> ptr const U` (recursive resolution via
            // `resolve_type_for_comptime_with_subst`).
            let return_type_sub = if let Some(&ty) = type_subst.get(&method_return_type_sym) {
                ty
            } else if return_type_for_call == Type::COMPTIME_TYPE {
                match self.resolve_type_for_comptime_with_subst(method_return_type_sym, &type_subst)
                {
                    Some(ty) => ty,
                    None => return_type_for_call,
                }
            } else {
                return_type_for_call
            };

            // Build the AIR call args: receiver first, then runtime args.
            let mut air_args = vec![AirCallArg {
                value: receiver_result.air_ref,
                mode: recv_pass_mode,
            }];
            air_args.extend(air_runtime_args);

            // Encode type args (raw Type discriminant values).
            let type_extra: Vec<u32> = type_args.iter().map(|t| t.as_u32()).collect();
            let type_args_start = air.add_extra(&type_extra);
            let type_args_len = type_args.len() as u32;

            // Encode runtime args.
            let mut args_extra = Vec::with_capacity(air_args.len() * 2);
            for arg in &air_args {
                args_extra.push(arg.value.as_u32());
                args_extra.push(arg.mode.as_u32());
            }
            let args_start_air = air.add_extra(&args_extra);
            let args_len_air = air_args.len() as u32;

            let call_name = format!("{}.{}", struct_name_str, method_name_str);
            let call_name_sym = self.interner.get_or_intern(&call_name);

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::CallGeneric {
                    name: call_name_sym,
                    type_args_start,
                    type_args_len,
                    args_start: args_start_air,
                    args_len: args_len_air,
                },
                ty: return_type_sub,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, return_type_sub));
        }

        let return_type = method_info.return_type;

        // Analyze arguments - receiver first, then remaining args
        let mut air_args = vec![AirCallArg {
            value: receiver_result.air_ref,
            mode: recv_pass_mode,
        }];
        air_args.extend(self.analyze_call_args(air, &args, ctx)?);

        // Generate a method call name: Type.method
        let call_name = format!("{}.{}", struct_name_str, method_name_str);
        let call_name_sym = self.interner.get_or_intern(&call_name);

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
                name: call_name_sym,
                args_start,
                args_len,
            },
            ty: return_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, return_type))
    }

    /// Analyze a module member call: `module.function(args)` becomes a direct function call.
    ///
    /// In Phase 1 of the module system, modules are virtual namespaces. When you import
    /// a module with `@import("foo.gruel")`, all of foo.gruel's functions are already in the
    /// global function table (via multi-file compilation). The module just provides a
    /// namespace at the source level.
    fn analyze_module_member_call_impl(
        &mut self,
        air: &mut Air,
        function_name: Spur,
        args: Vec<RirCallArg>,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Look up the function in the global function table
        let fn_name_str = self.interner.resolve(&function_name).to_string();
        let fn_info = *self
            .functions
            .get(&function_name)
            .ok_or_compile_error(ErrorKind::UndefinedFunction(fn_name_str.clone()), span)?;

        // Track this function as referenced (for lazy analysis)
        ctx.referenced_functions.insert(function_name);

        // Check visibility: private functions are only accessible from the same directory
        let accessing_file_id = span.file_id;
        let target_file_id = fn_info.file_id;
        if !self.is_accessible(accessing_file_id, target_file_id, fn_info.is_pub) {
            return Err(CompileError::new(
                ErrorKind::PrivateMemberAccess {
                    item_kind: "function".to_string(),
                    name: fn_name_str,
                },
                span,
            ));
        }

        // Get parameter data from the arena
        let param_types = self.param_arena.types(fn_info.params);
        let param_modes = self.param_arena.modes(fn_info.params);

        // Check argument count
        if args.len() != param_types.len() {
            let expected = param_types.len();
            let found = args.len();
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount { expected, found },
                span,
            ));
        }

        // Check that call-site argument modes match function parameter modes
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
                RirParamMode::Normal => {
                    // Normal params accept any mode
                }
                RirParamMode::Comptime => {
                    // Comptime params - handled elsewhere
                }
            }
        }

        // Analyze arguments
        let air_args = self.analyze_call_args(air, &args, ctx)?;

        // ADR-0056 Phase 4c: for any argument whose corresponding parameter
        // has an interface type, run a structural conformance check against
        // the argument's concrete type. If conformance succeeds we currently
        // surface "Phase 4d not yet implemented" — emitting a real
        // `MakeInterfaceRef` is wired in Phase 4d alongside codegen.
        let param_types_owned: Vec<Type> = self.param_arena.types(fn_info.params).to_vec();
        for (i, (arg_air, param_ty)) in air_args.iter().zip(param_types_owned.iter()).enumerate() {
            if let crate::types::TypeKind::Interface(iface_id) = param_ty.kind() {
                let arg_ty = air.get(arg_air.value).ty;
                let arg_span = self.rir.get(args[i].value).span;
                self.check_conforms(arg_ty, iface_id, arg_span)?;
                return Err(CompileError::new(
                    ErrorKind::InternalError(
                        "interface runtime dispatch (fat-pointer codegen) is not yet \
                         implemented (ADR-0056 Phase 4d). The conformance check passes; \
                         use `comptime T: I` for a working alternative."
                            .to_string(),
                    ),
                    arg_span,
                ));
            }
        }

        let return_type = fn_info.return_type;

        // Encode call args
        let mut extra_data = Vec::with_capacity(air_args.len() * 2);
        for arg in &air_args {
            extra_data.push(arg.value.as_u32());
            extra_data.push(arg.mode.as_u32());
        }
        let call_args_start = air.add_extra(&extra_data);
        let call_args_len = air_args.len() as u32;

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: function_name,
                args_start: call_args_start,
                args_len: call_args_len,
            },
            ty: return_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, return_type))
    }

    /// Implementation for AssocFnCall.
    pub(crate) fn analyze_assoc_fn_call_impl(
        &mut self,
        air: &mut Air,
        type_name: Spur,
        function: Spur,
        args: Vec<RirCallArg>,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let type_name_str = self.interner.resolve(&type_name).to_string();
        let function_name_str = self.interner.resolve(&function).to_string();

        // ADR-0063: `Ptr(T)::name(args)` / `MutPtr(T)::name(args)`. The RIR
        // path stores the LHS as the synthesized symbol `Ptr(T)`; sema's
        // resolve_type already handles type-call syntax via the
        // BuiltinTypeConstructor registry, so dispatch through there.
        if let Some((callee_name, _)) = crate::types::parse_type_call_syntax(&type_name_str)
            && gruel_builtins::get_builtin_type_constructor(&callee_name).is_some()
        {
            return self.dispatch_pointer_assoc_fn_call(
                air,
                type_name,
                &function_name_str,
                &args,
                span,
                ctx,
            );
        }

        // Check if this is an enum data variant construction (e.g., IntOption::Some(42)).
        // This must be checked before the struct lookup because enums and structs share the
        // same AssocFnCall syntax.
        if let Some(&enum_id) = self.enums.get(&type_name) {
            let enum_def = self.type_pool.enum_def(enum_id);
            if let Some(variant_index) = enum_def.find_variant(&function_name_str) {
                let variant_def = &enum_def.variants[variant_index];
                let field_types: Vec<Type> = variant_def.fields.clone();
                if !field_types.is_empty() {
                    // If this is a struct variant, error: use { } instead of ( )
                    if variant_def.is_struct_variant() {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: format!(
                                    "struct-style construction `{}::{} {{ ... }}`",
                                    type_name_str, function_name_str
                                ),
                                found: format!(
                                    "tuple-style construction `{}::{}(...)`",
                                    type_name_str, function_name_str
                                ),
                            },
                            span,
                        ));
                    }

                    // Check argument count
                    if args.len() != field_types.len() {
                        return Err(CompileError::new(
                            ErrorKind::WrongArgumentCount {
                                expected: field_types.len(),
                                found: args.len(),
                            },
                            span,
                        ));
                    }

                    // Analyze each argument and type-check against the variant's field types
                    let mut field_air_refs = Vec::with_capacity(args.len());
                    for (i, arg) in args.iter().enumerate() {
                        let result = self.analyze_inst(air, arg.value, ctx)?;
                        if result.ty != field_types[i] {
                            return Err(CompileError::new(
                                ErrorKind::TypeMismatch {
                                    expected: field_types[i].name().to_string(),
                                    found: result.ty.name().to_string(),
                                },
                                span,
                            ));
                        }
                        field_air_refs.push(result.air_ref.as_u32());
                    }

                    // Store field AirRefs in the extra array
                    let fields_len = field_air_refs.len() as u32;
                    let fields_start = air.add_extra(&field_air_refs);

                    let enum_type = Type::new_enum(enum_id);
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::EnumCreate {
                            enum_id,
                            variant_index: variant_index as u32,
                            fields_start,
                            fields_len,
                        },
                        ty: enum_type,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, enum_type));
                }
                // Unit variant called as a function: fall through to the error path.
            }

            // Not a variant — look for an associated function on this named enum (ADR-0053).
            let method_key = (enum_id, function);
            if let Some(method_info) = self.enum_methods.get(&method_key).copied() {
                ctx.referenced_methods
                    .insert((StructId(enum_id.0), function));

                if method_info.has_self {
                    return Err(CompileError::new(
                        ErrorKind::MethodCalledAsAssocFn {
                            type_name: type_name_str.clone(),
                            method_name: function_name_str.clone(),
                        },
                        span,
                    ));
                }

                let method_param_types: Vec<Type> =
                    self.param_arena.types(method_info.params).to_vec();
                if args.len() != method_param_types.len() {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: method_param_types.len(),
                            found: args.len(),
                        },
                        span,
                    ));
                }

                let mut extra_data = Vec::with_capacity(args.len() * 2);
                for (i, arg) in args.iter().enumerate() {
                    let result = self.analyze_inst(air, arg.value, ctx)?;
                    if result.ty != method_param_types[i] {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: method_param_types[i].name().to_string(),
                                found: result.ty.name().to_string(),
                            },
                            span,
                        ));
                    }
                    extra_data.push(result.air_ref.as_u32());
                    extra_data.push(AirArgMode::Normal.as_u32());
                }

                let enum_def = self.type_pool.enum_def(enum_id);
                let full_name = format!("{}::{}", enum_def.name, function_name_str);
                let callee_sym = self.interner.get_or_intern(&full_name);

                let args_start = air.add_extra(&extra_data);
                let args_len = args.len() as u32;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Call {
                        name: callee_sym,
                        args_start,
                        args_len,
                    },
                    ty: method_info.return_type,
                    span,
                });
                return Ok(AnalysisResult::new(air_ref, method_info.return_type));
            }
        }

        // Check if this is an enum data variant construction via a comptime type variable
        // (e.g., `let Opt = Option(i32); Opt::Some(42)`)
        if let Some(&ty) = ctx.comptime_type_vars.get(&type_name)
            && let TypeKind::Enum(enum_id) = ty.kind()
        {
            let enum_def = self.type_pool.enum_def(enum_id);
            if let Some(variant_index) = enum_def.find_variant(&function_name_str) {
                let variant_def = &enum_def.variants[variant_index];
                let field_types: Vec<Type> = variant_def.fields.clone();
                if !field_types.is_empty() {
                    if variant_def.is_struct_variant() {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: format!(
                                    "struct-style construction `{}::{} {{ ... }}`",
                                    type_name_str, function_name_str
                                ),
                                found: format!(
                                    "tuple-style construction `{}::{}(...)`",
                                    type_name_str, function_name_str
                                ),
                            },
                            span,
                        ));
                    }
                    if args.len() != field_types.len() {
                        return Err(CompileError::new(
                            ErrorKind::WrongArgumentCount {
                                expected: field_types.len(),
                                found: args.len(),
                            },
                            span,
                        ));
                    }
                    let mut field_air_refs = Vec::with_capacity(args.len());
                    for (i, arg) in args.iter().enumerate() {
                        let result = self.analyze_inst(air, arg.value, ctx)?;
                        if result.ty != field_types[i] {
                            return Err(CompileError::new(
                                ErrorKind::TypeMismatch {
                                    expected: field_types[i].name().to_string(),
                                    found: result.ty.name().to_string(),
                                },
                                span,
                            ));
                        }
                        field_air_refs.push(result.air_ref.as_u32());
                    }
                    let fields_len = field_air_refs.len() as u32;
                    let fields_start = air.add_extra(&field_air_refs);
                    let enum_type = Type::new_enum(enum_id);
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::EnumCreate {
                            enum_id,
                            variant_index: variant_index as u32,
                            fields_start,
                            fields_len,
                        },
                        ty: enum_type,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, enum_type));
                }
                // Unit variant called as function — fall through to error
            }

            // Not a variant — check for associated function on the enum
            let method_key = (enum_id, function);
            if let Some(method_info) = self.enum_methods.get(&method_key).copied() {
                ctx.referenced_methods
                    .insert((StructId(enum_id.0), function));

                if method_info.has_self {
                    return Err(CompileError::new(
                        ErrorKind::MethodCalledAsAssocFn {
                            type_name: type_name_str,
                            method_name: function_name_str,
                        },
                        span,
                    ));
                }

                let method_param_types: Vec<Type> =
                    self.param_arena.types(method_info.params).to_vec();
                if args.len() != method_param_types.len() {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: method_param_types.len(),
                            found: args.len(),
                        },
                        span,
                    ));
                }

                let mut extra_data = Vec::with_capacity(args.len() * 2);
                for (i, arg) in args.iter().enumerate() {
                    let result = self.analyze_inst(air, arg.value, ctx)?;
                    if result.ty != method_param_types[i] {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: method_param_types[i].name().to_string(),
                                found: result.ty.name().to_string(),
                            },
                            span,
                        ));
                    }
                    extra_data.push(result.air_ref.as_u32());
                    extra_data.push(AirArgMode::Normal.as_u32());
                }

                let enum_def = self.type_pool.enum_def(enum_id);
                let type_name_str2 = enum_def.name.clone();
                let full_name = format!("{}::{}", type_name_str2, function_name_str);
                let callee_sym = self.interner.get_or_intern(&full_name);

                let args_start = air.add_extra(&extra_data);
                let args_len = args.len() as u32;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Call {
                        name: callee_sym,
                        args_start,
                        args_len,
                    },
                    ty: method_info.return_type,
                    span,
                });
                return Ok(AnalysisResult::new(air_ref, method_info.return_type));
            }
        }

        // Check that the type exists and is a struct
        // First check if it's a comptime type variable (e.g., `let P = Point(); P::origin()`)
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
                .ok_or_compile_error(ErrorKind::UnknownType(type_name_str.clone()), span)?
        };

        // Handle builtin type associated functions
        if let Some(builtin_def) = self.get_builtin_type_def(struct_id) {
            return self.analyze_builtin_assoc_fn(
                air,
                ctx,
                (struct_id, builtin_def),
                &function_name_str,
                &args,
                span,
            );
        }

        // Look up the function using StructId
        let method_key = (struct_id, function);
        let method_info = self.methods.get(&method_key).ok_or_compile_error(
            ErrorKind::UndefinedAssocFn {
                type_name: type_name_str.clone(),
                function_name: function_name_str.clone(),
            },
            span,
        )?;

        // Track this associated function/method as referenced (for lazy analysis)
        ctx.referenced_methods.insert(method_key);

        // Check that this is an associated function (no self), not a method
        if method_info.has_self {
            return Err(CompileError::new(
                ErrorKind::MethodCalledAsAssocFn {
                    type_name: type_name_str,
                    method_name: function_name_str,
                },
                span,
            ));
        }

        // Check if calling an unchecked associated function requires a checked block
        if method_info.is_unchecked && ctx.checked_depth == 0 {
            return Err(CompileError::new(
                ErrorKind::UncheckedCallRequiresChecked(format!(
                    "{}::{}",
                    type_name_str, function_name_str
                )),
                span,
            ));
        }

        // Check argument count
        let method_param_types = self.param_arena.types(method_info.params);
        if args.len() != method_param_types.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: method_param_types.len(),
                    found: args.len(),
                },
                span,
            ));
        }

        // Check for exclusive access violation
        self.check_exclusive_access(&args, span)?;

        // Clone data needed before mutable borrow
        let return_type = method_info.return_type;

        // Analyze arguments
        let air_args = self.analyze_call_args(air, &args, ctx)?;

        // Generate a function call name: Type::function
        // Use the internal struct name (e.g., "__anon_struct_0") for anonymous structs,
        // not the user-visible type variable name (e.g., "P")
        let struct_def = self.type_pool.struct_def(struct_id);
        let internal_type_name = &struct_def.name;
        let call_name = format!("{}::{}", internal_type_name, function_name_str);
        let call_name_sym = self.interner.get_or_intern(&call_name);

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
                name: call_name_sym,
                args_start,
                args_len,
            },
            ty: return_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, return_type))
    }

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
            IntrinsicId::PtrRead => self.analyze_ptr_read_intrinsic(air, name, &args, span, ctx),
            IntrinsicId::PtrWrite => self.analyze_ptr_write_intrinsic(air, name, &args, span, ctx),
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
            | IntrinsicId::Conforms
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
            | IntrinsicId::SlicePtrMut => Err(CompileError::new(
                ErrorKind::UnknownIntrinsic(def.name.to_string()),
                span,
            )),

            // ADR-0064: build a slice from a raw pointer and a length.
            IntrinsicId::PartsToSlice => {
                self.analyze_parts_to_slice_intrinsic(air, &args, span, ctx, false)
            }
            IntrinsicId::PartsToMutSlice => {
                self.analyze_parts_to_slice_intrinsic(air, &args, span, ctx, true)
            }
        }
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
            let arg_result = self.analyze_inst(air, arg.value, ctx)?;
            let arg_type = arg_result.ty;

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
        air: &mut Air,
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

        // Verify the argument is a string literal
        let arg_inst = self.rir.get(args[0].value);
        if !matches!(&arg_inst.data, gruel_rir::InstData::StringConst(_)) {
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "@compile_error requires a string literal argument".into(),
                },
                arg_inst.span,
            ));
        }

        // Type is `!` (never) — @compile_error always terminates compilation
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::UnitConst,
            ty: Type::NEVER,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::NEVER))
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
        let struct_ty = value_result.ty;

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
            // Check if the file path ends with the import path (handles relative imports)
            if path.ends_with(import_path) {
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

    // Note: The old analyze_inst body from here onwards is now handled by the
    // dispatcher above and the category methods in analyze_ops.rs

    // ========================================================================
    // Helper methods for analysis
    // ========================================================================

    /// Analyze a binary arithmetic operator (+, -, *, /, %).
    ///
    /// Follows Rust's type inference rules:
    /// Types are determined by HM inference. Both operands must have the same type.
    fn analyze_binary_arith(
        &mut self,
        air: &mut Air,
        lhs: InstRef,
        rhs: InstRef,
        op: BinOp,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let lhs_result = self.analyze_inst(air, lhs, ctx)?;
        let rhs_result = self.analyze_inst(air, rhs, ctx)?;

        // Verify the type is numeric (HM should have enforced this, but check anyway)
        if !lhs_result.ty.is_numeric() && !lhs_result.ty.is_error() && !lhs_result.ty.is_never() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "numeric type".to_string(),
                    found: lhs_result.ty.name().to_string(),
                },
                span,
            ));
        }

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Bin(op, lhs_result.air_ref, rhs_result.air_ref),
            ty: lhs_result.ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, lhs_result.ty))
    }

    /// Analyze a comparison operator.
    ///
    /// Types are determined by HM inference. Both operands must have the same type.
    ///
    /// For equality operators (`==`, `!=`), both integers and booleans are allowed.
    /// For ordering operators (`<`, `>`, `<=`, `>=`), only integers are allowed.
    fn analyze_comparison(
        &mut self,
        air: &mut Air,
        (lhs, rhs): (InstRef, InstRef),
        allow_bool: bool,
        op: BinOp,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Check for chained comparisons (e.g., `a < b < c`)
        // Since the parser is left-associative, `a < b < c` parses as `(a < b) < c`,
        // so we only need to check if the LHS is a comparison.
        if self.is_comparison(lhs) {
            return Err(CompileError::new(ErrorKind::ChainedComparison, span)
                .with_help("use `&&` to combine comparisons: `a < b && b < c`"));
        }

        // Comparisons read values without consuming them (like projections).
        // This matches Rust's PartialEq trait which takes references.
        let lhs_result = self.analyze_inst_for_projection(air, lhs, ctx)?;
        let rhs_result = self.analyze_inst_for_projection(air, rhs, ctx)?;
        let lhs_type = lhs_result.ty;

        // Propagate Never/Error without additional type errors
        if lhs_type.is_never() || lhs_type.is_error() {
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Bin(op, lhs_result.air_ref, rhs_result.air_ref),
                ty: Type::BOOL,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::BOOL));
        }

        // Validate the type is appropriate for this comparison
        if allow_bool {
            // Equality operators (==, !=) work on integers, floats, booleans, strings, unit, and structs
            // Note: String is now a struct, so is_struct() covers it
            if !lhs_type.is_numeric()
                && lhs_type != Type::BOOL
                && lhs_type != Type::UNIT
                && !lhs_type.is_struct()
                && !self.is_builtin_string(lhs_type)
            {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "numeric, bool, string, unit, or struct".to_string(),
                        found: lhs_type.name().to_string(),
                    },
                    self.rir.get(lhs).span,
                ));
            }
        } else if !lhs_type.is_numeric() && !self.is_builtin_string(lhs_type) {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "numeric or string".to_string(),
                    found: lhs_type.name().to_string(),
                },
                self.rir.get(lhs).span,
            ));
        }

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Bin(op, lhs_result.air_ref, rhs_result.air_ref),
            ty: Type::BOOL,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::BOOL))
    }

    /// Try to evaluate an RIR expression as a compile-time constant.
    ///
    /// Returns `Some(value)` if the expression can be fully evaluated at compile time,
    /// or `None` if evaluation requires runtime information (e.g., variable values,
    /// function calls) or would cause overflow/panic.
    ///
    /// This is the foundation for compile-time bounds checking and can be extended
    /// for future `comptime` features.
    pub(crate) fn try_evaluate_const(&mut self, inst_ref: InstRef) -> Option<ConstValue> {
        let inst = self.rir.get(inst_ref);
        match &inst.data {
            // Integer literals
            InstData::IntConst(value) => i64::try_from(*value).ok().map(ConstValue::Integer),

            // Boolean literals
            InstData::BoolConst(value) => Some(ConstValue::Bool(*value)),

            InstData::Unary { op, operand } => {
                let v = self.try_evaluate_const(*operand)?;
                match (op, v) {
                    (UnaryOp::Neg, ConstValue::Integer(n)) => {
                        n.checked_neg().map(ConstValue::Integer)
                    }
                    (UnaryOp::Not, ConstValue::Bool(b)) => Some(ConstValue::Bool(!b)),
                    (UnaryOp::BitNot, ConstValue::Integer(n)) => Some(ConstValue::Integer(!n)),
                    _ => None,
                }
            }

            InstData::Bin { op, lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?;
                let r = self.try_evaluate_const(*rhs)?;
                match op {
                    BinOp::Add => l
                        .as_integer()?
                        .checked_add(r.as_integer()?)
                        .map(ConstValue::Integer),
                    BinOp::Sub => l
                        .as_integer()?
                        .checked_sub(r.as_integer()?)
                        .map(ConstValue::Integer),
                    BinOp::Mul => l
                        .as_integer()?
                        .checked_mul(r.as_integer()?)
                        .map(ConstValue::Integer),
                    BinOp::Div => {
                        let r = r.as_integer()?;
                        if r == 0 {
                            None
                        } else {
                            l.as_integer()?.checked_div(r).map(ConstValue::Integer)
                        }
                    }
                    BinOp::Mod => {
                        let r = r.as_integer()?;
                        if r == 0 {
                            None
                        } else {
                            l.as_integer()?.checked_rem(r).map(ConstValue::Integer)
                        }
                    }
                    BinOp::Eq => match (l, r) {
                        (ConstValue::Integer(a), ConstValue::Integer(b)) => {
                            Some(ConstValue::Bool(a == b))
                        }
                        (ConstValue::Bool(a), ConstValue::Bool(b)) => {
                            Some(ConstValue::Bool(a == b))
                        }
                        _ => None,
                    },
                    BinOp::Ne => match (l, r) {
                        (ConstValue::Integer(a), ConstValue::Integer(b)) => {
                            Some(ConstValue::Bool(a != b))
                        }
                        (ConstValue::Bool(a), ConstValue::Bool(b)) => {
                            Some(ConstValue::Bool(a != b))
                        }
                        _ => None,
                    },
                    BinOp::Lt => Some(ConstValue::Bool(l.as_integer()? < r.as_integer()?)),
                    BinOp::Gt => Some(ConstValue::Bool(l.as_integer()? > r.as_integer()?)),
                    BinOp::Le => Some(ConstValue::Bool(l.as_integer()? <= r.as_integer()?)),
                    BinOp::Ge => Some(ConstValue::Bool(l.as_integer()? >= r.as_integer()?)),
                    BinOp::And => Some(ConstValue::Bool(l.as_bool()? && r.as_bool()?)),
                    BinOp::Or => Some(ConstValue::Bool(l.as_bool()? || r.as_bool()?)),
                    BinOp::BitAnd => Some(ConstValue::Integer(l.as_integer()? & r.as_integer()?)),
                    BinOp::BitOr => Some(ConstValue::Integer(l.as_integer()? | r.as_integer()?)),
                    BinOp::BitXor => Some(ConstValue::Integer(l.as_integer()? ^ r.as_integer()?)),
                    // Only constant-fold small shift amounts to avoid type-width issues.
                    // For larger shifts, defer to runtime where hardware handles masking.
                    BinOp::Shl => {
                        let r = r.as_integer()?;
                        if !(0..8).contains(&r) {
                            return None;
                        }
                        Some(ConstValue::Integer(l.as_integer()? << r))
                    }
                    BinOp::Shr => {
                        let r = r.as_integer()?;
                        if !(0..8).contains(&r) {
                            return None;
                        }
                        Some(ConstValue::Integer(l.as_integer()? >> r))
                    }
                }
            }

            // Comptime block: comptime { expr } is compile-time evaluable if its inner expr is
            InstData::Comptime { expr } => self.try_evaluate_const(*expr),

            // Block: evaluate the result expression (last expression in the block)
            InstData::Block { extra_start, len } => {
                // A block is comptime-evaluable if it has a single instruction
                // (which is the result expression) OR if all statements are
                // side-effect-free and the result is comptime-evaluable.
                // For now, only handle the single-instruction case (common for
                // simple type-returning functions like `fn make_type() -> type { i32 }`).
                if *len == 1 {
                    let inst_refs = self.rir.get_extra(*extra_start, *len);
                    let result_ref = InstRef::from_raw(inst_refs[0]);
                    self.try_evaluate_const(result_ref)
                } else {
                    None // Blocks with multiple instructions need full interpreter support
                }
            }

            // Anonymous struct type: evaluate to a comptime type value
            InstData::AnonStructType {
                fields_start,
                fields_len,
                methods_start,
                methods_len,
                directives_start,
                directives_len,
            } => {
                // Get the field declarations from the RIR
                let field_decls = self.rir.get_field_decls(*fields_start, *fields_len);

                // Resolve each field type and build the struct fields
                let mut struct_fields = Vec::with_capacity(field_decls.len());
                for (name_sym, type_sym) in field_decls {
                    let name_str = self.interner.resolve(&name_sym).to_string();
                    // Try to resolve the type - for anonymous structs in comptime context,
                    // we need to be able to resolve the field types
                    let field_ty = self.resolve_type_for_comptime(type_sym)?;
                    struct_fields.push(StructField {
                        name: name_str,
                        ty: field_ty,
                    });
                }

                // Extract method signatures for structural equality comparison
                let method_sigs = self.extract_anon_method_sigs(*methods_start, *methods_len);

                // Find or create the anonymous struct type
                let (struct_ty, is_new) = self.find_or_create_anon_struct(
                    &struct_fields,
                    &method_sigs,
                    &HashMap::default(),
                );

                // Register methods if present and struct is new
                // This handles non-comptime functions like `fn Counter() -> type { struct { fn get() {} } }`
                // For comptime functions with captured values, use try_evaluate_const_with_subst instead
                if is_new && *methods_len > 0 {
                    let struct_id = struct_ty.as_struct()?;
                    // Use comptime-safe method registration (no type subst, no value subst for non-comptime)
                    self.register_anon_struct_methods_for_comptime_with_subst(
                        AnonStructSpec {
                            struct_id,
                            struct_type: struct_ty,
                            methods_start: *methods_start,
                            methods_len: *methods_len,
                        },
                        inst.span,
                        &HashMap::default(), // Empty type substitution
                        &HashMap::default(), // Empty value substitution (non-comptime)
                    )?;
                }
                // ADR-0058: splice `@derive(...)` directives on the anon
                // struct expression. Runs only on the new-StructId path so
                // identical parameterizations don't double-splice. Errors
                // are converted to None so the comptime evaluator gives
                // up on this value (matching the rest of the path).
                if is_new
                    && *directives_len > 0
                    && let Some(struct_id) = struct_ty.as_struct()
                    && let Err(e) = self.splice_anon_struct_derives(
                        struct_id,
                        *directives_start,
                        *directives_len,
                    )
                {
                    self.pending_anon_derive_errors.push(e);
                    return None;
                }
                Some(ConstValue::Type(struct_ty))
            }

            // Anonymous interface type (ADR-0057): evaluate to a comptime
            // type value carrying the freshly-built (or deduped) InterfaceId.
            InstData::AnonInterfaceType {
                methods_start,
                methods_len,
            } => {
                let methods = self
                    .build_anon_interface_def(
                        *methods_start,
                        *methods_len,
                        inst.span,
                        &HashMap::default(),
                    )
                    .ok()?;
                let iface_id = self.find_or_create_anon_interface(methods);
                Some(ConstValue::Type(Type::new_interface(iface_id)))
            }

            // Anonymous enum type: evaluate to a comptime type value
            InstData::AnonEnumType {
                variants_start,
                variants_len,
                methods_start,
                methods_len,
                directives_start,
                directives_len,
            } => {
                let variant_decls = self
                    .rir
                    .get_enum_variant_decls(*variants_start, *variants_len);

                let mut enum_variants = Vec::with_capacity(variant_decls.len());
                for (name_sym, field_type_syms, field_name_syms) in &variant_decls {
                    let name_str = self.interner.resolve(name_sym).to_string();
                    let mut fields = Vec::with_capacity(field_type_syms.len());
                    for ty_sym in field_type_syms {
                        let field_ty = self.resolve_type_for_comptime(*ty_sym)?;
                        fields.push(field_ty);
                    }
                    let field_names: Vec<String> = field_name_syms
                        .iter()
                        .map(|s| self.interner.resolve(s).to_string())
                        .collect();
                    enum_variants.push(EnumVariantDef {
                        name: name_str,
                        fields,
                        field_names,
                    });
                }

                let method_sigs = self.extract_anon_method_sigs(*methods_start, *methods_len);

                let (enum_ty, is_new) = self.find_or_create_anon_enum(
                    &enum_variants,
                    &method_sigs,
                    &HashMap::default(),
                );

                // Register methods for newly created anonymous enums
                if is_new
                    && *methods_len > 0
                    && let TypeKind::Enum(enum_id) = enum_ty.kind()
                {
                    self.register_anon_enum_methods_for_comptime_with_subst(
                        enum_id,
                        enum_ty,
                        *methods_start,
                        *methods_len,
                        &HashMap::default(),
                    );
                }
                // ADR-0058: splice `@derive(...)` on the anon enum
                // expression for the non-substitution comptime path.
                if is_new
                    && *directives_len > 0
                    && let TypeKind::Enum(enum_id) = enum_ty.kind()
                    && let Err(e) =
                        self.splice_anon_enum_derives(enum_id, *directives_start, *directives_len)
                {
                    self.pending_anon_derive_errors.push(e);
                    return None;
                }

                Some(ConstValue::Type(enum_ty))
            }

            // TypeConst: a type used as a value (e.g., `i32` in `identity(i32, 42)`)
            InstData::TypeConst { type_name } => {
                let type_name_str = self.interner.resolve(type_name);
                let ty = match type_name_str {
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
                    "!" => Type::NEVER,
                    _ => {
                        // Check for struct types
                        if let Some(&struct_id) = self.structs.get(type_name) {
                            Type::new_struct(struct_id)
                        } else if let Some(&enum_id) = self.enums.get(type_name) {
                            Type::new_enum(enum_id)
                        } else {
                            return None; // Unknown type
                        }
                    }
                };
                Some(ConstValue::Type(ty))
            }

            // VarRef: when a variable reference is actually a type name (e.g., `Point` in `fn make_type() -> type { Point }`)
            InstData::VarRef { name } => {
                // Try to resolve as a type - if it's a type name, return the type
                let name_str = self.interner.resolve(name);
                let ty = match name_str {
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
                    "!" => Type::NEVER,
                    _ => {
                        // Check for struct types
                        if let Some(&struct_id) = self.structs.get(name) {
                            Type::new_struct(struct_id)
                        } else if let Some(&enum_id) = self.enums.get(name) {
                            Type::new_enum(enum_id)
                        } else {
                            return None; // Not a type name - can't evaluate at compile time
                        }
                    }
                };
                Some(ConstValue::Type(ty))
            }

            // Everything else requires runtime evaluation
            _ => None,
        }
    }

    /// Try to extract a constant integer value from an RIR index expression.
    ///
    /// This is used for compile-time bounds checking. Returns `Some(value)` if
    /// the index can be evaluated to an integer constant at compile time.
    pub(crate) fn try_get_const_index(&mut self, inst_ref: InstRef) -> Option<i64> {
        self.try_evaluate_const(inst_ref)?.as_integer()
    }

    /// Try to evaluate an RIR instruction to a compile-time constant value with type substitution.
    ///
    /// This is used when evaluating generic functions that return `type`. For example,
    /// when calling `fn Pair(comptime T: type) -> type { struct { first: T, second: T } }`
    /// with `Pair(i32)`, we need to substitute `T -> i32` when evaluating the body.
    ///
    /// The `type_subst` map contains mappings from type parameter names to concrete types.
    pub(crate) fn try_evaluate_const_with_subst(
        &mut self,
        inst_ref: InstRef,
        type_subst: &rustc_hash::FxHashMap<Spur, Type>,
        value_subst: &rustc_hash::FxHashMap<Spur, ConstValue>,
    ) -> Option<ConstValue> {
        let inst = self.rir.get(inst_ref);
        match &inst.data {
            // Integer literals
            InstData::IntConst(value) => i64::try_from(*value).ok().map(ConstValue::Integer),

            // Boolean literals
            InstData::BoolConst(value) => Some(ConstValue::Bool(*value)),

            InstData::Unary { op, operand } => {
                let v = self.try_evaluate_const_with_subst(*operand, type_subst, value_subst)?;
                match (op, v) {
                    (UnaryOp::Neg, ConstValue::Integer(n)) => {
                        n.checked_neg().map(ConstValue::Integer)
                    }
                    (UnaryOp::Not, ConstValue::Bool(b)) => Some(ConstValue::Bool(!b)),
                    (UnaryOp::BitNot, ConstValue::Integer(n)) => Some(ConstValue::Integer(!n)),
                    _ => None,
                }
            }

            InstData::Bin { op, lhs, rhs } => {
                let l = self.try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?;
                let r = self.try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?;
                match op {
                    BinOp::Add => l
                        .as_integer()?
                        .checked_add(r.as_integer()?)
                        .map(ConstValue::Integer),
                    BinOp::Sub => l
                        .as_integer()?
                        .checked_sub(r.as_integer()?)
                        .map(ConstValue::Integer),
                    BinOp::Mul => l
                        .as_integer()?
                        .checked_mul(r.as_integer()?)
                        .map(ConstValue::Integer),
                    BinOp::Div => {
                        let r = r.as_integer()?;
                        if r == 0 {
                            None
                        } else {
                            l.as_integer()?.checked_div(r).map(ConstValue::Integer)
                        }
                    }
                    BinOp::Mod => {
                        let r = r.as_integer()?;
                        if r == 0 {
                            None
                        } else {
                            l.as_integer()?.checked_rem(r).map(ConstValue::Integer)
                        }
                    }
                    BinOp::Eq => match (l, r) {
                        (ConstValue::Integer(a), ConstValue::Integer(b)) => {
                            Some(ConstValue::Bool(a == b))
                        }
                        (ConstValue::Bool(a), ConstValue::Bool(b)) => {
                            Some(ConstValue::Bool(a == b))
                        }
                        _ => None,
                    },
                    BinOp::Ne => match (l, r) {
                        (ConstValue::Integer(a), ConstValue::Integer(b)) => {
                            Some(ConstValue::Bool(a != b))
                        }
                        (ConstValue::Bool(a), ConstValue::Bool(b)) => {
                            Some(ConstValue::Bool(a != b))
                        }
                        _ => None,
                    },
                    BinOp::Lt => Some(ConstValue::Bool(l.as_integer()? < r.as_integer()?)),
                    BinOp::Gt => Some(ConstValue::Bool(l.as_integer()? > r.as_integer()?)),
                    BinOp::Le => Some(ConstValue::Bool(l.as_integer()? <= r.as_integer()?)),
                    BinOp::Ge => Some(ConstValue::Bool(l.as_integer()? >= r.as_integer()?)),
                    BinOp::And => Some(ConstValue::Bool(l.as_bool()? && r.as_bool()?)),
                    BinOp::Or => Some(ConstValue::Bool(l.as_bool()? || r.as_bool()?)),
                    BinOp::BitAnd => Some(ConstValue::Integer(l.as_integer()? & r.as_integer()?)),
                    BinOp::BitOr => Some(ConstValue::Integer(l.as_integer()? | r.as_integer()?)),
                    BinOp::BitXor => Some(ConstValue::Integer(l.as_integer()? ^ r.as_integer()?)),
                    BinOp::Shl => {
                        let r = r.as_integer()?;
                        if !(0..8).contains(&r) {
                            return None;
                        }
                        Some(ConstValue::Integer(l.as_integer()? << r))
                    }
                    BinOp::Shr => {
                        let r = r.as_integer()?;
                        if !(0..8).contains(&r) {
                            return None;
                        }
                        Some(ConstValue::Integer(l.as_integer()? >> r))
                    }
                }
            }

            // Comptime block: comptime { expr } is compile-time evaluable if its inner expr is
            InstData::Comptime { expr } => {
                self.try_evaluate_const_with_subst(*expr, type_subst, value_subst)
            }

            // Block: evaluate the result expression (last expression in the block)
            InstData::Block { extra_start, len } => {
                if *len == 1 {
                    let inst_refs = self.rir.get_extra(*extra_start, *len);
                    let result_ref = InstRef::from_raw(inst_refs[0]);
                    self.try_evaluate_const_with_subst(result_ref, type_subst, value_subst)
                } else {
                    None
                }
            }

            // Anonymous struct type: evaluate to a comptime type value with substitution
            InstData::AnonStructType {
                fields_start,
                fields_len,
                methods_start,
                methods_len,
                directives_start,
                directives_len,
            } => {
                let field_decls = self.rir.get_field_decls(*fields_start, *fields_len);

                let mut struct_fields = Vec::with_capacity(field_decls.len());
                for (name_sym, type_sym) in field_decls {
                    let name_str = self.interner.resolve(&name_sym).to_string();
                    // Use the substitution-aware type resolution
                    let field_ty =
                        self.resolve_type_for_comptime_with_subst(type_sym, type_subst)?;
                    struct_fields.push(StructField {
                        name: name_str,
                        ty: field_ty,
                    });
                }

                // Extract method signatures for structural equality comparison
                let method_sigs = self.extract_anon_method_sigs(*methods_start, *methods_len);

                let (struct_ty, is_new) =
                    self.find_or_create_anon_struct(&struct_fields, &method_sigs, value_subst);

                // Register methods if present (requires preview feature)
                // Register if either:
                // 1. This is a newly created struct (is_new=true), OR
                // 2. The struct exists but has no methods registered yet
                if *methods_len > 0 {
                    let struct_id = struct_ty.as_struct()?;

                    // Check if methods are already registered for this struct
                    let method_refs = self.rir.get_inst_refs(*methods_start, *methods_len);
                    let first_method_ref = method_refs[0];
                    let first_method_inst = self.rir.get(first_method_ref);
                    if let InstData::FnDecl {
                        name: method_name, ..
                    } = &first_method_inst.data
                    {
                        let needs_registration =
                            !self.methods.contains_key(&(struct_id, *method_name));

                        if needs_registration {
                            // Use comptime-safe method registration with type substitution
                            self.register_anon_struct_methods_for_comptime_with_subst(
                                AnonStructSpec {
                                    struct_id,
                                    struct_type: struct_ty,
                                    methods_start: *methods_start,
                                    methods_len: *methods_len,
                                },
                                inst.span,
                                type_subst,
                                value_subst,
                            )?;
                        }
                    }
                }
                // ADR-0058: splice `@derive(...)` on the anon struct
                // expression for parameterized comptime calls. Each fresh
                // `StructId` (per parameterization) gets its own splice.
                if is_new
                    && *directives_len > 0
                    && let Some(struct_id) = struct_ty.as_struct()
                    && let Err(e) = self.splice_anon_struct_derives(
                        struct_id,
                        *directives_start,
                        *directives_len,
                    )
                {
                    self.pending_anon_derive_errors.push(e);
                    return None;
                }
                Some(ConstValue::Type(struct_ty))
            }

            // Anonymous interface type with substitution (ADR-0057):
            // resolve method-sig types under `type_subst` (and the regular
            // comptime-resolver path for `T` references) before deduping.
            // `value_subst` is unused — interfaces have no captured values.
            InstData::AnonInterfaceType {
                methods_start,
                methods_len,
            } => {
                let methods = self
                    .build_anon_interface_def(*methods_start, *methods_len, inst.span, type_subst)
                    .ok()?;
                let _ = value_subst;
                let iface_id = self.find_or_create_anon_interface(methods);
                Some(ConstValue::Type(Type::new_interface(iface_id)))
            }

            // Anonymous enum type: evaluate to a comptime type value with substitution
            InstData::AnonEnumType {
                variants_start,
                variants_len,
                methods_start,
                methods_len,
                directives_start,
                directives_len,
            } => {
                let variant_decls = self
                    .rir
                    .get_enum_variant_decls(*variants_start, *variants_len);

                let mut enum_variants = Vec::with_capacity(variant_decls.len());
                for (name_sym, field_type_syms, field_name_syms) in &variant_decls {
                    let name_str = self.interner.resolve(name_sym).to_string();
                    let mut fields = Vec::with_capacity(field_type_syms.len());
                    for ty_sym in field_type_syms {
                        let field_ty =
                            self.resolve_type_for_comptime_with_subst(*ty_sym, type_subst)?;
                        fields.push(field_ty);
                    }
                    let field_names: Vec<String> = field_name_syms
                        .iter()
                        .map(|s| self.interner.resolve(s).to_string())
                        .collect();
                    enum_variants.push(EnumVariantDef {
                        name: name_str,
                        fields,
                        field_names,
                    });
                }

                let method_sigs = self.extract_anon_method_sigs(*methods_start, *methods_len);

                let (enum_ty, is_new) =
                    self.find_or_create_anon_enum(&enum_variants, &method_sigs, value_subst);

                // Register methods for newly created anonymous enums with captured values
                if is_new
                    && *methods_len > 0
                    && let TypeKind::Enum(enum_id) = enum_ty.kind()
                {
                    self.register_anon_enum_methods_for_comptime_with_subst(
                        enum_id,
                        enum_ty,
                        *methods_start,
                        *methods_len,
                        type_subst,
                    );
                }
                // ADR-0058: splice `@derive(...)` on the anon enum
                // expression with substitution. Each fresh `EnumId` gets
                // its own splice.
                if is_new
                    && *directives_len > 0
                    && let TypeKind::Enum(enum_id) = enum_ty.kind()
                    && let Err(e) =
                        self.splice_anon_enum_derives(enum_id, *directives_start, *directives_len)
                {
                    self.pending_anon_derive_errors.push(e);
                    return None;
                }

                Some(ConstValue::Type(enum_ty))
            }

            // TypeConst: a type used as a value
            InstData::TypeConst { type_name } => {
                // First check the substitution map
                if let Some(&ty) = type_subst.get(type_name) {
                    return Some(ConstValue::Type(ty));
                }

                let type_name_str = self.interner.resolve(type_name);
                let ty = match type_name_str {
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
                    "!" => Type::NEVER,
                    _ => {
                        if let Some(&struct_id) = self.structs.get(type_name) {
                            Type::new_struct(struct_id)
                        } else if let Some(&enum_id) = self.enums.get(type_name) {
                            Type::new_enum(enum_id)
                        } else {
                            return None;
                        }
                    }
                };
                Some(ConstValue::Type(ty))
            }

            // VarRef: check substitution maps first, then try as a type name
            InstData::VarRef { name } => {
                // Check if this is a type parameter in the type substitution map
                if let Some(&ty) = type_subst.get(name) {
                    return Some(ConstValue::Type(ty));
                }

                // Check if this is a comptime value variable in the value substitution map
                if let Some(value) = value_subst.get(name) {
                    return Some(*value);
                }

                // Try to resolve as a type name
                let name_str = self.interner.resolve(name);
                let ty = match name_str {
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
                    "!" => Type::NEVER,
                    _ => {
                        if let Some(&struct_id) = self.structs.get(name) {
                            Type::new_struct(struct_id)
                        } else if let Some(&enum_id) = self.enums.get(name) {
                            Type::new_enum(enum_id)
                        } else {
                            return None;
                        }
                    }
                };
                Some(ConstValue::Type(ty))
            }

            // Everything else requires runtime evaluation
            _ => None,
        }
    }

    // =========================================================================
    // Phase 1a: Stateful Comptime Interpreter
    // =========================================================================
    //
    // The interpreter extends `try_evaluate_const` with mutable local variable
    // state, enabling:
    //   - Multi-statement `comptime { ... }` blocks
    //   - `let` bindings within comptime blocks
    //   - Variable assignment within comptime blocks
    //   - `if`/`else` with comptime-evaluable conditions
    //   - `while`, `loop`, `break`, `continue` (Phase 1b)
    //   - Function calls, push/pop call frames (Phase 1c)
    //   - `ConstValue::Struct`, `ConstValue::Array` (Phase 1d)
    //   - Comptime arg evaluation via full interpreter (Phase 1e)

    /// Try to evaluate a single expression as a comptime argument value.
    ///
    /// Used when validating and extracting values for `comptime` parameters at
    /// call sites (Phase 1e). First tries the lightweight non-stateful evaluator
    /// (fast for literals and arithmetic), then falls back to the full stateful
    /// interpreter which supports function calls and composite operations.
    ///
    /// Returns `Some(value)` if evaluable at compile time, `None` otherwise.
    /// Never returns control-flow signals (`BreakSignal`, `ContinueSignal`,
    /// `ReturnSignal`).
    pub(crate) fn try_evaluate_comptime_arg(
        &mut self,
        inst_ref: InstRef,
        ctx: &AnalysisContext,
        outer_span: Span,
    ) -> Option<ConstValue> {
        // Fast path: lightweight evaluator handles literals and arithmetic.
        if let Some(val) = self.try_evaluate_const(inst_ref) {
            return Some(val);
        }
        // Full stateful interpreter: supports function calls, let bindings, etc.
        // Save and restore step counter so arg evaluation doesn't consume the
        // budget of any outer comptime block that may be in progress.
        let prev_steps = self.comptime_steps_used;
        self.comptime_steps_used = 0;
        let mut locals = ctx.comptime_value_vars.clone();
        let result = self
            .evaluate_comptime_inst(inst_ref, &mut locals, ctx, outer_span)
            .ok();
        self.comptime_steps_used = prev_steps;
        // Filter out control-flow signals — they cannot be meaningful here.
        result.filter(|v| {
            !matches!(
                v,
                ConstValue::BreakSignal | ConstValue::ContinueSignal | ConstValue::ReturnSignal
            )
        })
    }

    /// Format a comptime value as a human-readable string.
    ///
    /// Used by `@dbg` and `@compileLog` to render comptime values during
    /// compile-time evaluation.
    fn format_const_value(&self, val: ConstValue, span: Span) -> CompileResult<String> {
        match val {
            ConstValue::Bool(b) => Ok(if b {
                "true".to_string()
            } else {
                "false".to_string()
            }),
            ConstValue::Integer(v) => Ok(format!("{v}")),
            ConstValue::Unit => Ok("()".to_string()),
            ConstValue::ComptimeStr(idx) => match &self.comptime_heap[idx as usize] {
                ComptimeHeapItem::String(s) => Ok(s.clone()),
                _ => Err(CompileError::new(
                    ErrorKind::ComptimeEvaluationFailed {
                        reason: "invalid comptime_str heap reference".into(),
                    },
                    span,
                )),
            },
            _ => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "expression contains values that cannot be known at compile time"
                        .into(),
                },
                span,
            )),
        }
    }

    /// Resolve a `ConstValue::ComptimeStr` to its Rust string content.
    pub(crate) fn resolve_comptime_str(&self, idx: u32, span: Span) -> CompileResult<&str> {
        match &self.comptime_heap[idx as usize] {
            ComptimeHeapItem::String(s) => Ok(s.as_str()),
            _ => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "invalid comptime_str heap reference".into(),
                },
                span,
            )),
        }
    }

    /// Evaluate a `comptime_str` method call in the comptime interpreter.
    ///
    /// Dispatches methods like `len`, `is_empty`, `contains`, `starts_with`,
    /// `ends_with`, `eq`, `ne`, `lt`, `le`, `gt`, `ge`, and `concat`.
    fn evaluate_comptime_str_method(
        &mut self,
        str_idx: u32,
        method_name: &str,
        call_args: &[gruel_rir::RirCallArg],
        locals: &mut HashMap<Spur, ConstValue>,
        ctx: &AnalysisContext,
        spans: ComptimeSpans,
    ) -> CompileResult<ConstValue> {
        let ComptimeSpans {
            outer: outer_span,
            inst: inst_span,
        } = spans;
        let s = self.resolve_comptime_str(str_idx, inst_span)?.to_string();

        match method_name {
            "len" => {
                if !call_args.is_empty() {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicWrongArgCount {
                            name: "len".to_string(),
                            expected: 0,
                            found: call_args.len(),
                        },
                        inst_span,
                    ));
                }
                Ok(ConstValue::Integer(s.len() as i64))
            }
            "is_empty" => {
                if !call_args.is_empty() {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicWrongArgCount {
                            name: "is_empty".to_string(),
                            expected: 0,
                            found: call_args.len(),
                        },
                        inst_span,
                    ));
                }
                Ok(ConstValue::Bool(s.is_empty()))
            }
            "contains" | "starts_with" | "ends_with" | "eq" | "ne" | "lt" | "le" | "gt" | "ge" => {
                if call_args.len() != 1 {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicWrongArgCount {
                            name: method_name.to_string(),
                            expected: 1,
                            found: call_args.len(),
                        },
                        inst_span,
                    ));
                }
                let arg_val =
                    self.evaluate_comptime_inst(call_args[0].value, locals, ctx, outer_span)?;
                let other_idx = match arg_val {
                    ConstValue::ComptimeStr(idx) => idx,
                    _ => {
                        return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: format!(
                                    "comptime_str.{method_name} expects a comptime_str argument"
                                ),
                            },
                            inst_span,
                        ));
                    }
                };
                let other = self.resolve_comptime_str(other_idx, inst_span)?.to_string();
                let result = match method_name {
                    "contains" => s.contains(other.as_str()),
                    "starts_with" => s.starts_with(other.as_str()),
                    "ends_with" => s.ends_with(other.as_str()),
                    "eq" => s == other,
                    "ne" => s != other,
                    "lt" => s < other,
                    "le" => s <= other,
                    "gt" => s > other,
                    "ge" => s >= other,
                    _ => unreachable!(),
                };
                Ok(ConstValue::Bool(result))
            }
            "concat" => {
                if call_args.len() != 1 {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicWrongArgCount {
                            name: "concat".to_string(),
                            expected: 1,
                            found: call_args.len(),
                        },
                        inst_span,
                    ));
                }
                let arg_val =
                    self.evaluate_comptime_inst(call_args[0].value, locals, ctx, outer_span)?;
                let other_idx = match arg_val {
                    ConstValue::ComptimeStr(idx) => idx,
                    _ => {
                        return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: "comptime_str.concat expects a comptime_str argument"
                                    .into(),
                            },
                            inst_span,
                        ));
                    }
                };
                let other = self.resolve_comptime_str(other_idx, inst_span)?.to_string();
                let result = format!("{s}{other}");
                let idx = self.comptime_heap.len() as u32;
                self.comptime_heap.push(ComptimeHeapItem::String(result));
                Ok(ConstValue::ComptimeStr(idx))
            }
            "clone" => {
                if !call_args.is_empty() {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicWrongArgCount {
                            name: "clone".to_string(),
                            expected: 0,
                            found: call_args.len(),
                        },
                        inst_span,
                    ));
                }
                Ok(self.alloc_comptime_str(s))
            }
            "push_str" | "push" | "clear" | "reserve" => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "cannot call .{method_name}() on a compile-time string; use .concat() to produce a new string"
                    ),
                },
                inst_span,
            )),
            "capacity" => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "capacity is not available for compile-time strings".into(),
                },
                inst_span,
            )),
            _ => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!("unknown comptime_str method '{method_name}'"),
                },
                inst_span,
            )),
        }
    }

    /// Evaluate a comptime intrinsic argument as a string.
    ///
    /// Accepts both `StringConst` instructions (string literals) and
    /// `ConstValue::ComptimeStr` values from comptime evaluation.
    fn evaluate_comptime_string_arg(
        &mut self,
        arg_ref: InstRef,
        locals: &mut HashMap<Spur, ConstValue>,
        ctx: &AnalysisContext,
        outer_span: Span,
    ) -> CompileResult<String> {
        let arg_inst = self.rir.get(arg_ref);
        // Try string literal first
        if let gruel_rir::InstData::StringConst(spur) = &arg_inst.data {
            return Ok(self.interner.resolve(spur).to_string());
        }
        // Otherwise evaluate as a comptime expression
        let val = self.evaluate_comptime_inst(arg_ref, locals, ctx, outer_span)?;
        match val {
            ConstValue::ComptimeStr(idx) => self
                .resolve_comptime_str(idx, arg_inst.span)
                .map(|s| s.to_string()),
            _ => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "@compile_error requires a string literal or comptime_str argument"
                        .into(),
                },
                arg_inst.span,
            )),
        }
    }

    /// Allocate a `comptime_str` on the comptime heap and return a `ConstValue::ComptimeStr`.
    fn alloc_comptime_str(&mut self, s: String) -> ConstValue {
        let idx = self.comptime_heap.len() as u32;
        self.comptime_heap.push(ComptimeHeapItem::String(s));
        ConstValue::ComptimeStr(idx)
    }

    /// Allocate a comptime struct on the heap and return a `ConstValue::Struct`.
    fn alloc_comptime_struct(
        &mut self,
        struct_id: StructId,
        fields: Vec<ConstValue>,
    ) -> ConstValue {
        let idx = self.comptime_heap.len() as u32;
        self.comptime_heap
            .push(ComptimeHeapItem::Struct { struct_id, fields });
        ConstValue::Struct(idx)
    }

    /// Allocate a comptime array on the heap and return a `ConstValue::Array`.
    fn alloc_comptime_array(&mut self, elements: Vec<ConstValue>) -> ConstValue {
        let idx = self.comptime_heap.len() as u32;
        self.comptime_heap.push(ComptimeHeapItem::Array(elements));
        ConstValue::Array(idx)
    }

    /// Resolve a `TypeKind` variant name to its discriminant index.
    fn typekind_variant_idx(&self, variant_name: &str) -> u32 {
        let enum_id = self
            .builtin_typekind_id
            .expect("TypeKind enum not injected");
        let enum_def = self.type_pool.enum_def(enum_id);
        enum_def
            .find_variant(variant_name)
            .unwrap_or_else(|| panic!("TypeKind variant '{variant_name}' not found")) as u32
    }

    /// Evaluate `@type_name(T)` — returns the type's name as a `comptime_str`.
    fn evaluate_comptime_type_name(&mut self, ty: Type, _span: Span) -> CompileResult<ConstValue> {
        let name = self.type_pool.format_type_name(ty);
        Ok(self.alloc_comptime_str(name))
    }

    /// Evaluate `@type_info(T)` — returns a comptime struct describing the type.
    fn evaluate_comptime_type_info(&mut self, ty: Type, span: Span) -> CompileResult<ConstValue> {
        let typekind_enum_id = self
            .builtin_typekind_id
            .expect("TypeKind enum not injected");
        let typekind_type = Type::new_enum(typekind_enum_id);

        match ty.kind() {
            TypeKind::Struct(struct_id) => {
                self.build_struct_type_info(struct_id, typekind_enum_id, typekind_type)
            }
            TypeKind::Enum(enum_id) => {
                self.build_enum_type_info(enum_id, typekind_enum_id, typekind_type)
            }
            TypeKind::I8 => {
                self.build_int_type_info("i8", 8, true, typekind_enum_id, typekind_type)
            }
            TypeKind::I16 => {
                self.build_int_type_info("i16", 16, true, typekind_enum_id, typekind_type)
            }
            TypeKind::I32 => {
                self.build_int_type_info("i32", 32, true, typekind_enum_id, typekind_type)
            }
            TypeKind::I64 => {
                self.build_int_type_info("i64", 64, true, typekind_enum_id, typekind_type)
            }
            TypeKind::U8 => {
                self.build_int_type_info("u8", 8, false, typekind_enum_id, typekind_type)
            }
            TypeKind::U16 => {
                self.build_int_type_info("u16", 16, false, typekind_enum_id, typekind_type)
            }
            TypeKind::U32 => {
                self.build_int_type_info("u32", 32, false, typekind_enum_id, typekind_type)
            }
            TypeKind::U64 => {
                self.build_int_type_info("u64", 64, false, typekind_enum_id, typekind_type)
            }
            TypeKind::Bool => {
                self.build_simple_type_info("bool", "Bool", typekind_enum_id, typekind_type)
            }
            TypeKind::Unit => {
                self.build_simple_type_info("()", "Unit", typekind_enum_id, typekind_type)
            }
            TypeKind::Never => {
                self.build_simple_type_info("!", "Never", typekind_enum_id, typekind_type)
            }
            TypeKind::Array(array_type_id) => {
                let (elem_ty, len) = self.type_pool.array_def(array_type_id);
                let elem_name = self.type_pool.format_type_name(elem_ty);
                let name = format!("[{elem_name}; {len}]");
                self.build_simple_type_info(&name, "Array", typekind_enum_id, typekind_type)
            }
            _ => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!("@type_info not supported for type '{}'", ty.name()),
                },
                span,
            )),
        }
    }

    /// Build a simple type info struct with just `kind` and `name` fields.
    fn build_simple_type_info(
        &mut self,
        type_name: &str,
        kind_variant_name: &str,
        typekind_enum_id: EnumId,
        typekind_type: Type,
    ) -> CompileResult<ConstValue> {
        let kind_val = ConstValue::EnumVariant {
            enum_id: typekind_enum_id,
            variant_idx: self.typekind_variant_idx(kind_variant_name),
        };
        let name_val = self.alloc_comptime_str(type_name.to_string());

        let fields = vec![
            StructField {
                name: "kind".to_string(),
                ty: typekind_type,
            },
            StructField {
                name: "name".to_string(),
                ty: Type::COMPTIME_STR,
            },
        ];
        let (info_type, _) = self.find_or_create_anon_struct(&fields, &[], &HashMap::default());
        let info_struct_id = match info_type.kind() {
            TypeKind::Struct(id) => id,
            _ => unreachable!(),
        };

        Ok(self.alloc_comptime_struct(info_struct_id, vec![kind_val, name_val]))
    }

    /// Build type info for an integer type (includes `bits` and `is_signed`).
    fn build_int_type_info(
        &mut self,
        type_name: &str,
        bits: i32,
        is_signed: bool,
        typekind_enum_id: EnumId,
        typekind_type: Type,
    ) -> CompileResult<ConstValue> {
        let kind_val = ConstValue::EnumVariant {
            enum_id: typekind_enum_id,
            variant_idx: self.typekind_variant_idx("Int"),
        };
        let name_val = self.alloc_comptime_str(type_name.to_string());

        let fields = vec![
            StructField {
                name: "kind".to_string(),
                ty: typekind_type,
            },
            StructField {
                name: "name".to_string(),
                ty: Type::COMPTIME_STR,
            },
            StructField {
                name: "bits".to_string(),
                ty: Type::I32,
            },
            StructField {
                name: "is_signed".to_string(),
                ty: Type::BOOL,
            },
        ];
        let (info_type, _) = self.find_or_create_anon_struct(&fields, &[], &HashMap::default());
        let info_struct_id = match info_type.kind() {
            TypeKind::Struct(id) => id,
            _ => unreachable!(),
        };

        Ok(self.alloc_comptime_struct(
            info_struct_id,
            vec![
                kind_val,
                name_val,
                ConstValue::Integer(bits as i64),
                ConstValue::Bool(is_signed),
            ],
        ))
    }

    /// Build type info for a struct type (includes `field_count` and `fields` array).
    fn build_struct_type_info(
        &mut self,
        struct_id: StructId,
        typekind_enum_id: EnumId,
        typekind_type: Type,
    ) -> CompileResult<ConstValue> {
        let kind_val = ConstValue::EnumVariant {
            enum_id: typekind_enum_id,
            variant_idx: self.typekind_variant_idx("Struct"),
        };

        // Get struct info
        let struct_def = self.type_pool.struct_def(struct_id);
        let struct_name = struct_def.name.clone();
        let field_defs: Vec<(String, Type)> = struct_def
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty))
            .collect();
        let field_count = field_defs.len() as i32;

        let name_val = self.alloc_comptime_str(struct_name);

        // Create FieldInfo struct type
        let field_info_fields = vec![
            StructField {
                name: "name".to_string(),
                ty: Type::COMPTIME_STR,
            },
            StructField {
                name: "field_type".to_string(),
                ty: Type::COMPTIME_TYPE,
            },
        ];
        let (field_info_type, _) =
            self.find_or_create_anon_struct(&field_info_fields, &[], &HashMap::default());
        let field_info_struct_id = match field_info_type.kind() {
            TypeKind::Struct(id) => id,
            _ => unreachable!(),
        };

        // Create FieldInfo instances for each field
        let mut field_values = Vec::with_capacity(field_defs.len());
        for (fname, ftype) in &field_defs {
            let fname_val = self.alloc_comptime_str(fname.clone());
            let ftype_val = ConstValue::Type(*ftype);
            let field_info =
                self.alloc_comptime_struct(field_info_struct_id, vec![fname_val, ftype_val]);
            field_values.push(field_info);
        }

        // Create the fields array
        let fields_array = self.alloc_comptime_array(field_values);

        // Create the array type for fields: [FieldInfo; N]
        let fields_array_type =
            Type::new_array(self.get_or_create_array_type(field_info_type, field_count as u64));

        // Create the info struct type
        let info_fields = vec![
            StructField {
                name: "kind".to_string(),
                ty: typekind_type,
            },
            StructField {
                name: "name".to_string(),
                ty: Type::COMPTIME_STR,
            },
            StructField {
                name: "field_count".to_string(),
                ty: Type::I32,
            },
            StructField {
                name: "fields".to_string(),
                ty: fields_array_type,
            },
        ];
        let (info_type, _) =
            self.find_or_create_anon_struct(&info_fields, &[], &HashMap::default());
        let info_struct_id = match info_type.kind() {
            TypeKind::Struct(id) => id,
            _ => unreachable!(),
        };

        Ok(self.alloc_comptime_struct(
            info_struct_id,
            vec![
                kind_val,
                name_val,
                ConstValue::Integer(field_count as i64),
                fields_array,
            ],
        ))
    }

    /// Build type info for an enum type (includes `variant_count` and `variants` array).
    fn build_enum_type_info(
        &mut self,
        enum_id: EnumId,
        typekind_enum_id: EnumId,
        typekind_type: Type,
    ) -> CompileResult<ConstValue> {
        let kind_val = ConstValue::EnumVariant {
            enum_id: typekind_enum_id,
            variant_idx: self.typekind_variant_idx("Enum"),
        };

        // Get enum info
        let enum_def = self.type_pool.enum_def(enum_id);
        let enum_name = enum_def.name.clone();
        let variant_defs: Vec<(String, Vec<(String, Type)>)> = enum_def
            .variants
            .iter()
            .map(|v| {
                let vfields: Vec<(String, Type)> = if v.is_struct_variant() {
                    // Struct variant: field names + types
                    v.field_names
                        .iter()
                        .zip(v.fields.iter())
                        .map(|(name, ty)| (name.clone(), *ty))
                        .collect()
                } else {
                    // Unit or tuple variant: just types with positional names
                    v.fields
                        .iter()
                        .enumerate()
                        .map(|(i, ty)| (format!("{i}"), *ty))
                        .collect()
                };
                (v.name.clone(), vfields)
            })
            .collect();
        let variant_count = variant_defs.len() as i32;

        let name_val = self.alloc_comptime_str(enum_name);

        // Create FieldInfo struct type (reuse if already exists)
        let field_info_fields = vec![
            StructField {
                name: "name".to_string(),
                ty: Type::COMPTIME_STR,
            },
            StructField {
                name: "field_type".to_string(),
                ty: Type::COMPTIME_TYPE,
            },
        ];
        let (field_info_type, _) =
            self.find_or_create_anon_struct(&field_info_fields, &[], &HashMap::default());
        let field_info_struct_id = match field_info_type.kind() {
            TypeKind::Struct(id) => id,
            _ => unreachable!(),
        };

        // Create VariantInfo instances
        let mut variant_values = Vec::with_capacity(variant_defs.len());
        for (vname, vfields) in &variant_defs {
            let vname_val = self.alloc_comptime_str(vname.clone());

            // Create FieldInfo array for this variant's fields
            let mut vfield_values = Vec::new();
            for (fname, ftype) in vfields {
                let fname_val = self.alloc_comptime_str(fname.clone());
                let ftype_val = ConstValue::Type(*ftype);
                let field_info =
                    self.alloc_comptime_struct(field_info_struct_id, vec![fname_val, ftype_val]);
                vfield_values.push(field_info);
            }
            let vfields_array = self.alloc_comptime_array(vfield_values);
            let vfield_count = vfields.len() as i32;

            // Create VariantInfo struct type with fields array
            let vfields_array_type = Type::new_array(
                self.get_or_create_array_type(field_info_type, vfields.len() as u64),
            );
            let variant_info_fields = vec![
                StructField {
                    name: "name".to_string(),
                    ty: Type::COMPTIME_STR,
                },
                StructField {
                    name: "field_count".to_string(),
                    ty: Type::I32,
                },
                StructField {
                    name: "fields".to_string(),
                    ty: vfields_array_type,
                },
            ];
            let (variant_info_type, _) =
                self.find_or_create_anon_struct(&variant_info_fields, &[], &HashMap::default());
            let variant_info_struct_id = match variant_info_type.kind() {
                TypeKind::Struct(id) => id,
                _ => unreachable!(),
            };

            let variant_info = self.alloc_comptime_struct(
                variant_info_struct_id,
                vec![
                    vname_val,
                    ConstValue::Integer(vfield_count as i64),
                    vfields_array,
                ],
            );
            variant_values.push(variant_info);
        }

        // Create the variants array
        let variants_array = self.alloc_comptime_array(variant_values);

        // For the array type, we need a VariantInfo type — use one for unit variants (0 fields)
        let empty_fields_array_type =
            Type::new_array(self.get_or_create_array_type(field_info_type, 0));
        let variant_info_fields = vec![
            StructField {
                name: "name".to_string(),
                ty: Type::COMPTIME_STR,
            },
            StructField {
                name: "field_count".to_string(),
                ty: Type::I32,
            },
            StructField {
                name: "fields".to_string(),
                ty: empty_fields_array_type,
            },
        ];
        let (variant_info_type, _) =
            self.find_or_create_anon_struct(&variant_info_fields, &[], &HashMap::default());
        let variants_array_type =
            Type::new_array(self.get_or_create_array_type(variant_info_type, variant_count as u64));

        // Create the info struct type
        let info_fields = vec![
            StructField {
                name: "kind".to_string(),
                ty: typekind_type,
            },
            StructField {
                name: "name".to_string(),
                ty: Type::COMPTIME_STR,
            },
            StructField {
                name: "variant_count".to_string(),
                ty: Type::I32,
            },
            StructField {
                name: "variants".to_string(),
                ty: variants_array_type,
            },
        ];
        let (info_type, _) =
            self.find_or_create_anon_struct(&info_fields, &[], &HashMap::default());
        let info_struct_id = match info_type.kind() {
            TypeKind::Struct(id) => id,
            _ => unreachable!(),
        };

        Ok(self.alloc_comptime_struct(
            info_struct_id,
            vec![
                kind_val,
                name_val,
                ConstValue::Integer(variant_count as i64),
                variants_array,
            ],
        ))
    }

    /// Evaluate a comptime block expression using the stateful interpreter.
    ///
    /// Seeds the local scope from `ctx.comptime_value_vars` (captured comptime
    /// parameters, e.g. `N` in `FixedBuffer(comptime N: i32)`), then delegates
    /// to `evaluate_comptime_inst`.
    fn evaluate_comptime_block(
        &mut self,
        inst_ref: InstRef,
        ctx: &AnalysisContext,
        span: Span,
    ) -> CompileResult<ConstValue> {
        let _span = info_span!("comptime").entered();
        // Reset the step counter and heap for this comptime block evaluation.
        self.comptime_steps_used = 0;
        self.comptime_heap.clear();
        // Seed interpreter locals with any comptime-captured values from the
        // outer analysis context (e.g. `N` in a method of `FixedBuffer(N)`).
        let mut locals = ctx.comptime_value_vars.clone();
        let result = self.evaluate_comptime_inst(inst_ref, &mut locals, ctx, span)?;
        // Control-flow signals escaping the top level are errors.
        // BreakSignal/ContinueSignal mean break/continue outside a loop.
        // ReturnSignal means return outside a function (comptime block is not a function).
        match result {
            ConstValue::BreakSignal | ConstValue::ContinueSignal => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "break/continue outside a loop in comptime block".into(),
                },
                span,
            )),
            ConstValue::ReturnSignal => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "return outside a function in comptime block".into(),
                },
                span,
            )),
            val => Ok(val),
        }
    }

    /// Evaluate a comptime expression at the top level, outside any function body.
    ///
    /// Used during Phase 2.5 const-initializer evaluation, where there is no
    /// enclosing `AnalysisContext`. Builds a minimal stub context (no locals,
    /// no comptime type params) and delegates to [`evaluate_comptime_block`].
    pub(crate) fn evaluate_comptime_top_level(
        &mut self,
        inst_ref: InstRef,
        span: Span,
    ) -> CompileResult<ConstValue> {
        let empty_params: Vec<ParamInfo> = Vec::new();
        let empty_resolved: HashMap<InstRef, Type> = HashMap::default();
        let stub = AnalysisContext {
            locals: HashMap::default(),
            params: &empty_params,
            next_slot: 0,
            loop_depth: 0,
            forbid_break: None,
            checked_depth: 0,
            used_locals: HashSet::default(),
            return_type: Type::UNIT,
            scope_stack: Vec::new(),
            resolved_types: &empty_resolved,
            moved_vars: HashMap::default(),
            warnings: Vec::new(),
            local_string_table: HashMap::default(),
            local_strings: Vec::new(),
            local_bytes: Vec::new(),
            comptime_type_vars: HashMap::default(),
            comptime_value_vars: HashMap::default(),
            referenced_functions: HashSet::default(),
            referenced_methods: HashSet::default(),
        };
        self.evaluate_comptime_block(inst_ref, &stub, span)
    }

    /// Evaluate a comptime expression without clearing the heap.
    ///
    /// This is used by `@field` and other intrinsics inside `comptime_unroll for` bodies
    /// where the heap contains data from the iterable evaluation that must be preserved.
    fn evaluate_comptime_expr(
        &mut self,
        inst_ref: InstRef,
        ctx: &AnalysisContext,
        span: Span,
    ) -> CompileResult<ConstValue> {
        let prev_steps = self.comptime_steps_used;
        self.comptime_steps_used = 0;
        let mut locals = ctx.comptime_value_vars.clone();
        let result = self.evaluate_comptime_inst(inst_ref, &mut locals, ctx, span)?;
        self.comptime_steps_used = prev_steps;
        match result {
            ConstValue::BreakSignal | ConstValue::ContinueSignal => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "break/continue outside a loop in comptime expression".into(),
                },
                span,
            )),
            ConstValue::ReturnSignal => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "return outside a function in comptime expression".into(),
                },
                span,
            )),
            val => Ok(val),
        }
    }

    /// Recursively evaluate one RIR instruction in a comptime context.
    ///
    /// `locals` holds variables declared within the current comptime block.
    /// Returns the evaluated `ConstValue`, or a `CompileError` if the
    /// instruction is not compile-time evaluable.
    fn evaluate_comptime_inst(
        &mut self,
        inst_ref: InstRef,
        locals: &mut HashMap<Spur, ConstValue>,
        ctx: &AnalysisContext,
        outer_span: Span,
    ) -> CompileResult<ConstValue> {
        // Clone the instruction data up-front to release the `self.rir` borrow
        // before any recursive calls to `evaluate_comptime_inst`.
        let (inst_span, inst_data) = {
            let inst = self.rir.get(inst_ref);
            (inst.span, inst.data.clone())
        };

        /// Return a "cannot be known at compile time" error at `span`.
        #[inline]
        fn not_const(span: Span) -> CompileError {
            CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "expression contains values that cannot be known at compile time"
                        .into(),
                },
                span,
            )
        }

        /// Return an arithmetic overflow error at `span`.
        #[inline]
        fn overflow(span: Span) -> CompileError {
            CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "arithmetic overflow in comptime evaluation".into(),
                },
                span,
            )
        }

        /// Extract integer from ConstValue, or return not_const error.
        #[inline]
        fn int(v: ConstValue, span: Span) -> CompileResult<i64> {
            v.as_integer().ok_or_else(|| not_const(span))
        }

        /// Extract bool from ConstValue, or return not_const error.
        #[inline]
        fn bool_val(v: ConstValue, span: Span) -> CompileResult<bool> {
            v.as_bool().ok_or_else(|| not_const(span))
        }

        match inst_data {
            // ── Literals ──────────────────────────────────────────────────────
            InstData::IntConst(value) => {
                i64::try_from(value).map(ConstValue::Integer).map_err(|_| {
                    CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: "integer constant too large for comptime evaluation".into(),
                        },
                        inst_span,
                    )
                })
            }

            InstData::BoolConst(value) => Ok(ConstValue::Bool(value)),

            InstData::UnitConst => Ok(ConstValue::Unit),

            InstData::StringConst(spur) => {
                let s = self.interner.resolve(&spur).to_string();
                let idx = self.comptime_heap.len() as u32;
                self.comptime_heap.push(ComptimeHeapItem::String(s));
                Ok(ConstValue::ComptimeStr(idx))
            }

            InstData::Unary { op, operand } => {
                let v = self.evaluate_comptime_inst(operand, locals, ctx, outer_span)?;
                match op {
                    UnaryOp::Neg => {
                        let n = int(v, inst_span)?;
                        n.checked_neg()
                            .map(ConstValue::Integer)
                            .ok_or_else(|| overflow(inst_span))
                    }
                    UnaryOp::Not => Ok(ConstValue::Bool(!bool_val(v, inst_span)?)),
                    UnaryOp::BitNot => Ok(ConstValue::Integer(!int(v, inst_span)?)),
                }
            }

            InstData::Bin { op, lhs, rhs } => {
                let lv = self.evaluate_comptime_inst(lhs, locals, ctx, outer_span)?;
                let rv = self.evaluate_comptime_inst(rhs, locals, ctx, outer_span)?;
                let div_zero = |what: &str| {
                    CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!("{} by zero in comptime evaluation", what),
                        },
                        inst_span,
                    )
                };
                let shift_oob = || {
                    CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: "shift amount out of range in comptime evaluation".into(),
                        },
                        inst_span,
                    )
                };
                match op {
                    BinOp::Add => int(lv, inst_span)?
                        .checked_add(int(rv, inst_span)?)
                        .map(ConstValue::Integer)
                        .ok_or_else(|| overflow(inst_span)),
                    BinOp::Sub => int(lv, inst_span)?
                        .checked_sub(int(rv, inst_span)?)
                        .map(ConstValue::Integer)
                        .ok_or_else(|| overflow(inst_span)),
                    BinOp::Mul => int(lv, inst_span)?
                        .checked_mul(int(rv, inst_span)?)
                        .map(ConstValue::Integer)
                        .ok_or_else(|| overflow(inst_span)),
                    BinOp::Div => {
                        let r = int(rv, inst_span)?;
                        if r == 0 {
                            return Err(div_zero("division"));
                        }
                        int(lv, inst_span)?
                            .checked_div(r)
                            .map(ConstValue::Integer)
                            .ok_or_else(|| overflow(inst_span))
                    }
                    BinOp::Mod => {
                        let r = int(rv, inst_span)?;
                        if r == 0 {
                            return Err(div_zero("modulo"));
                        }
                        int(lv, inst_span)?
                            .checked_rem(r)
                            .map(ConstValue::Integer)
                            .ok_or_else(|| overflow(inst_span))
                    }
                    BinOp::Eq | BinOp::Ne => {
                        let eq = match (lv, rv) {
                            (ConstValue::Integer(a), ConstValue::Integer(b)) => a == b,
                            (ConstValue::Bool(a), ConstValue::Bool(b)) => a == b,
                            (ConstValue::ComptimeStr(a), ConstValue::ComptimeStr(b)) => {
                                let sa = self.resolve_comptime_str(a, inst_span)?;
                                let sb = self.resolve_comptime_str(b, inst_span)?;
                                sa == sb
                            }
                            (
                                ConstValue::EnumVariant {
                                    enum_id: ae,
                                    variant_idx: av,
                                },
                                ConstValue::EnumVariant {
                                    enum_id: be,
                                    variant_idx: bv,
                                },
                            ) => ae == be && av == bv,
                            _ => return Err(not_const(inst_span)),
                        };
                        Ok(ConstValue::Bool(if matches!(op, BinOp::Eq) {
                            eq
                        } else {
                            !eq
                        }))
                    }
                    BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                        let ord = match (lv, rv) {
                            (ConstValue::Integer(a), ConstValue::Integer(b)) => a.cmp(&b),
                            (ConstValue::ComptimeStr(a), ConstValue::ComptimeStr(b)) => {
                                let sa = self.resolve_comptime_str(a, inst_span)?;
                                let sb = self.resolve_comptime_str(b, inst_span)?;
                                sa.cmp(sb)
                            }
                            _ => return Err(not_const(inst_span)),
                        };
                        use std::cmp::Ordering::*;
                        let result = matches!(
                            (op, ord),
                            (BinOp::Lt, Less)
                                | (BinOp::Gt, Greater)
                                | (BinOp::Le, Less | Equal)
                                | (BinOp::Ge, Greater | Equal)
                        );
                        Ok(ConstValue::Bool(result))
                    }
                    BinOp::And => Ok(ConstValue::Bool(
                        bool_val(lv, inst_span)? && bool_val(rv, inst_span)?,
                    )),
                    BinOp::Or => Ok(ConstValue::Bool(
                        bool_val(lv, inst_span)? || bool_val(rv, inst_span)?,
                    )),
                    BinOp::BitAnd => Ok(ConstValue::Integer(
                        int(lv, inst_span)? & int(rv, inst_span)?,
                    )),
                    BinOp::BitOr => Ok(ConstValue::Integer(
                        int(lv, inst_span)? | int(rv, inst_span)?,
                    )),
                    BinOp::BitXor => Ok(ConstValue::Integer(
                        int(lv, inst_span)? ^ int(rv, inst_span)?,
                    )),
                    BinOp::Shl => {
                        let r = int(rv, inst_span)?;
                        if !(0..64).contains(&r) {
                            return Err(shift_oob());
                        }
                        Ok(ConstValue::Integer(int(lv, inst_span)? << r))
                    }
                    BinOp::Shr => {
                        let r = int(rv, inst_span)?;
                        if !(0..64).contains(&r) {
                            return Err(shift_oob());
                        }
                        Ok(ConstValue::Integer(int(lv, inst_span)? >> r))
                    }
                }
            }

            // ── Block: iterate instructions, return last value ─────────────────
            InstData::Block { extra_start, len } => {
                // Collect into owned Vec to release the `self.rir` borrow before
                // the loop body calls `evaluate_comptime_inst` recursively.
                let raw_refs: Vec<u32> = self.rir.get_extra(extra_start, len).to_vec();
                let mut last_val = ConstValue::Unit;
                for raw_ref in raw_refs {
                    last_val = self.evaluate_comptime_inst(
                        InstRef::from_raw(raw_ref),
                        locals,
                        ctx,
                        outer_span,
                    )?;
                    // Propagate control-flow signals immediately — don't execute
                    // remaining statements after a break, continue, or return.
                    if matches!(
                        last_val,
                        ConstValue::BreakSignal
                            | ConstValue::ContinueSignal
                            | ConstValue::ReturnSignal
                    ) {
                        return Ok(last_val);
                    }
                }
                Ok(last_val)
            }

            // ── Variable declaration ──────────────────────────────────────────
            InstData::Alloc { name, init, .. } => {
                let val = self.evaluate_comptime_inst(init, locals, ctx, outer_span)?;
                if let Some(name_sym) = name {
                    locals.insert(name_sym, val);
                }
                Ok(ConstValue::Unit)
            }

            // ── Variable reference ────────────────────────────────────────────
            InstData::VarRef { name } => {
                // 1. Locals declared within this comptime block (or seeded from outer captures).
                if let Some(&val) = locals.get(&name) {
                    return Ok(val);
                }
                // 2. Comptime type overrides (type params bound during generic function calls).
                if let Some(&ty) = self.comptime_type_overrides.get(&name) {
                    return Ok(ConstValue::Type(ty));
                }
                // 3. Comptime type variables from the outer analysis context
                //    (e.g. `let P = make_point()` in the enclosing function).
                if let Some(&ty) = ctx.comptime_type_vars.get(&name) {
                    return Ok(ConstValue::Type(ty));
                }
                // 4. Built-in type names used as values (e.g. `i32` in `identity(i32, 42)`).
                let name_str = self.interner.resolve(&name).to_string();
                let builtin_ty = match name_str.as_str() {
                    "i8" => Some(Type::I8),
                    "i16" => Some(Type::I16),
                    "i32" => Some(Type::I32),
                    "i64" => Some(Type::I64),
                    "u8" => Some(Type::U8),
                    "u16" => Some(Type::U16),
                    "u32" => Some(Type::U32),
                    "u64" => Some(Type::U64),
                    "bool" => Some(Type::BOOL),
                    "()" => Some(Type::UNIT),
                    "!" => Some(Type::NEVER),
                    _ => None,
                };
                if let Some(ty) = builtin_ty {
                    return Ok(ConstValue::Type(ty));
                }
                // 5. User-defined struct/enum types used as values.
                if let Some(&struct_id) = self.structs.get(&name) {
                    return Ok(ConstValue::Type(Type::new_struct(struct_id)));
                }
                if let Some(&enum_id) = self.enums.get(&name) {
                    return Ok(ConstValue::Type(Type::new_enum(enum_id)));
                }
                // 6. Not a known comptime value — must be a runtime variable.
                Err(not_const(inst_span))
            }

            // ── Assignment ────────────────────────────────────────────────────
            InstData::Assign { name, value } => {
                let val = self.evaluate_comptime_inst(value, locals, ctx, outer_span)?;
                locals.insert(name, val);
                Ok(ConstValue::Unit)
            }

            // ── Branch (if/else) ──────────────────────────────────────────────
            InstData::Branch {
                cond,
                then_block,
                else_block,
            } => {
                let cond_val = self.evaluate_comptime_inst(cond, locals, ctx, outer_span)?;
                match cond_val {
                    ConstValue::Bool(true) => {
                        self.evaluate_comptime_inst(then_block, locals, ctx, outer_span)
                    }
                    ConstValue::Bool(false) => {
                        if let Some(else_ref) = else_block {
                            self.evaluate_comptime_inst(else_ref, locals, ctx, outer_span)
                        } else {
                            Ok(ConstValue::Unit)
                        }
                    }
                    _ => Err(not_const(inst_span)),
                }
            }

            // ── Nested comptime ───────────────────────────────────────────────
            InstData::Comptime { expr } => {
                self.evaluate_comptime_inst(expr, locals, ctx, outer_span)
            }

            // ── Declarations are no-ops in comptime context ───────────────────
            InstData::FnDecl { .. }
            | InstData::DropFnDecl { .. }
            | InstData::ConstDecl { .. }
            | InstData::StructDecl { .. }
            | InstData::EnumDecl { .. } => Ok(ConstValue::Unit),

            // ── Type-related: delegate to existing evaluator ──────────────────
            // AnonStructType and TypeConst need the full try_evaluate_const
            // logic (type registry lookups, structural equality, etc.).
            InstData::AnonStructType { .. }
            | InstData::AnonEnumType { .. }
            | InstData::TypeConst { .. } => self
                .try_evaluate_const(inst_ref)
                .ok_or_else(|| not_const(inst_span)),

            // ── While loop ────────────────────────────────────────────────────
            // `while cond { body }` — evaluates until condition is false.
            InstData::Loop { cond, body } => {
                const COMPTIME_MAX_STEPS: u64 = 1_000_000;
                loop {
                    let cond_val = self.evaluate_comptime_inst(cond, locals, ctx, outer_span)?;
                    if !bool_val(cond_val, inst_span)? {
                        break;
                    }
                    self.comptime_steps_used += 1;
                    if self.comptime_steps_used > COMPTIME_MAX_STEPS {
                        return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: format!(
                                    "comptime evaluation exceeded step budget of {} iterations",
                                    COMPTIME_MAX_STEPS
                                ),
                            },
                            inst_span,
                        ));
                    }
                    match self.evaluate_comptime_inst(body, locals, ctx, outer_span)? {
                        ConstValue::BreakSignal => break,
                        ConstValue::ContinueSignal => continue,
                        _ => {}
                    }
                }
                Ok(ConstValue::Unit)
            }

            // ── For-in loop ──────────────────────────────────────────────────
            // Not supported in comptime context (desugared to while at runtime).
            InstData::For { .. } => Err(not_const(inst_span)),

            // ── Infinite loop ─────────────────────────────────────────────────
            // `loop { body }` — runs until a break (or step budget exceeded).
            InstData::InfiniteLoop { body } => {
                const COMPTIME_MAX_STEPS: u64 = 1_000_000;
                loop {
                    self.comptime_steps_used += 1;
                    if self.comptime_steps_used > COMPTIME_MAX_STEPS {
                        return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: format!(
                                    "comptime evaluation exceeded step budget of {} iterations",
                                    COMPTIME_MAX_STEPS
                                ),
                            },
                            inst_span,
                        ));
                    }
                    match self.evaluate_comptime_inst(body, locals, ctx, outer_span)? {
                        ConstValue::BreakSignal => break,
                        ConstValue::ContinueSignal => continue,
                        _ => {}
                    }
                }
                Ok(ConstValue::Unit)
            }

            // ── Break / Continue ──────────────────────────────────────────────
            InstData::Break => Ok(ConstValue::BreakSignal),
            InstData::Continue => Ok(ConstValue::ContinueSignal),

            // ── Return ────────────────────────────────────────────────────────
            // `return expr` or bare `return` inside a comptime function.
            // Stores the return value in a side channel then signals the Call handler.
            InstData::Ret(opt_ref) => {
                let return_val = match opt_ref {
                    Some(val_ref) => {
                        self.evaluate_comptime_inst(val_ref, locals, ctx, outer_span)?
                    }
                    None => ConstValue::Unit,
                };
                self.comptime_return_value = Some(return_val);
                Ok(ConstValue::ReturnSignal)
            }

            // ── Function call ─────────────────────────────────────────────────
            // Evaluate the callee's body with the arguments bound as locals.
            InstData::Call {
                name,
                args_start,
                args_len,
            } => {
                const COMPTIME_CALL_DEPTH_LIMIT: u32 = 64;

                // Look up the function in the function table.
                let fn_info = match self.functions.get(&name) {
                    Some(info) => *info,
                    None => return Err(not_const(inst_span)),
                };

                // Evaluate all arguments before entering the callee frame.
                let call_args = self.rir.get_call_args(args_start, args_len);
                let mut arg_values = Vec::with_capacity(call_args.len());
                for call_arg in &call_args {
                    let val =
                        self.evaluate_comptime_inst(call_arg.value, locals, ctx, outer_span)?;
                    arg_values.push(val);
                }

                // For generic functions, extract type parameter bindings from
                // comptime arguments and set them as type overrides so that
                // struct/enum resolution inside the callee body can find them.
                let param_comptime = self.param_arena.comptime(fn_info.params).to_vec();
                let param_names = self.param_arena.names(fn_info.params).to_vec();
                let mut type_overrides: HashMap<Spur, Type> = HashMap::default();
                if fn_info.is_generic {
                    for (i, is_comptime) in param_comptime.iter().enumerate() {
                        if *is_comptime && let Some(ConstValue::Type(ty)) = arg_values.get(i) {
                            type_overrides.insert(param_names[i], *ty);
                        }
                    }
                }

                // Enforce call stack depth limit to prevent infinite recursion.
                if self.comptime_call_depth >= COMPTIME_CALL_DEPTH_LIMIT {
                    return Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "comptime call stack depth exceeded {} levels (possible infinite recursion)",
                                COMPTIME_CALL_DEPTH_LIMIT
                            ),
                        },
                        inst_span,
                    ));
                }

                // Bind non-comptime parameters to argument values in a fresh call frame.
                // Comptime (type) parameters are not bound as locals — they are
                // available through comptime_type_overrides.
                let mut call_locals: HashMap<Spur, ConstValue> = {
                    let mut m = HashMap::default();
                    m.reserve(param_names.len());
                    m
                };
                for (i, (param_name, arg_val)) in
                    param_names.iter().zip(arg_values.iter()).enumerate()
                {
                    if !param_comptime.get(i).copied().unwrap_or(false) {
                        call_locals.insert(*param_name, *arg_val);
                    }
                }

                // Push type overrides for the duration of this call.
                let saved_overrides =
                    std::mem::replace(&mut self.comptime_type_overrides, type_overrides);

                // Execute the callee body.
                self.comptime_call_depth += 1;
                let body_result =
                    self.evaluate_comptime_inst(fn_info.body, &mut call_locals, ctx, outer_span);
                self.comptime_call_depth -= 1;

                // Restore previous type overrides.
                self.comptime_type_overrides = saved_overrides;

                let body_result = body_result?;

                // Consume any return signal; fall through on plain values.
                match body_result {
                    ConstValue::ReturnSignal => {
                        // `return val` was executed — take the stored value.
                        self.comptime_return_value.take().ok_or_else(|| {
                            CompileError::new(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: "comptime return signal missing its value".into(),
                                },
                                inst_span,
                            )
                        })
                    }
                    ConstValue::BreakSignal | ConstValue::ContinueSignal => {
                        // break/continue escaped a function body — syntax error in Gruel.
                        Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: "break/continue outside a loop in comptime function".into(),
                            },
                            inst_span,
                        ))
                    }
                    val => Ok(val),
                }
            }

            // ── Struct construction ───────────────────────────────────────────
            InstData::StructInit {
                module,
                type_name,
                fields_start,
                fields_len,
            } => {
                // Module-qualified struct literals are not supported in comptime.
                if module.is_some() {
                    return Err(not_const(inst_span));
                }

                // Resolve the struct type by name (also checks comptime type overrides).
                let struct_id = match self.resolve_comptime_struct(type_name, ctx) {
                    Some(id) => id,
                    None => return Err(not_const(inst_span)),
                };

                // Get the struct definition to know field declaration order.
                let struct_def = self.type_pool.struct_def(struct_id);
                let field_count = struct_def.fields.len();

                // Build a map from field name string to declaration index.
                let field_index_map: rustc_hash::FxHashMap<String, usize> = struct_def
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, f)| (f.name.clone(), i))
                    .collect();

                // Retrieve field initializers from the RIR (may be in any source order).
                let field_inits = self.rir.get_field_inits(fields_start, fields_len);

                // Evaluate each field expression and place it in declaration order.
                let mut field_values = vec![ConstValue::Unit; field_count];
                for (field_name_sym, field_value_ref) in &field_inits {
                    let field_name = self.interner.resolve(field_name_sym).to_string();
                    let idx = match field_index_map.get(&field_name) {
                        Some(&i) => i,
                        None => return Err(not_const(inst_span)),
                    };
                    let val =
                        self.evaluate_comptime_inst(*field_value_ref, locals, ctx, outer_span)?;
                    field_values[idx] = val;
                }

                // Allocate a new heap item and return its index.
                let heap_idx = self.comptime_heap.len() as u32;
                self.comptime_heap.push(ComptimeHeapItem::Struct {
                    struct_id,
                    fields: field_values,
                });
                Ok(ConstValue::Struct(heap_idx))
            }

            // ── Field access ──────────────────────────────────────────────────
            InstData::FieldGet { base, field } => {
                let base_val = self.evaluate_comptime_inst(base, locals, ctx, outer_span)?;
                match base_val {
                    ConstValue::Struct(heap_idx) => {
                        // Clone data out to release the heap borrow before calling struct_def.
                        let (struct_id, fields) = match &self.comptime_heap[heap_idx as usize] {
                            ComptimeHeapItem::Struct { struct_id, fields } => {
                                (*struct_id, fields.clone())
                            }
                            _ => return Err(not_const(inst_span)),
                        };
                        let struct_def = self.type_pool.struct_def(struct_id);
                        let field_name = self.interner.resolve(&field);
                        let (field_idx, _) =
                            struct_def.find_field(field_name).ok_or_else(|| {
                                CompileError::new(
                                    ErrorKind::ComptimeEvaluationFailed {
                                        reason: format!(
                                            "unknown field '{}' in comptime struct",
                                            field_name
                                        ),
                                    },
                                    inst_span,
                                )
                            })?;
                        Ok(fields[field_idx])
                    }
                    _ => Err(not_const(inst_span)),
                }
            }

            // ── Array construction ────────────────────────────────────────────
            InstData::ArrayInit {
                elems_start,
                elems_len,
            } => {
                let elem_refs = self.rir.get_inst_refs(elems_start, elems_len);
                let mut elem_values = Vec::with_capacity(elem_refs.len());
                for elem_ref in &elem_refs {
                    let val = self.evaluate_comptime_inst(*elem_ref, locals, ctx, outer_span)?;
                    elem_values.push(val);
                }
                let heap_idx = self.comptime_heap.len() as u32;
                self.comptime_heap
                    .push(ComptimeHeapItem::Array(elem_values));
                Ok(ConstValue::Array(heap_idx))
            }

            // ── Array index read ──────────────────────────────────────────────
            InstData::IndexGet { base, index } => {
                let base_val = self.evaluate_comptime_inst(base, locals, ctx, outer_span)?;
                let index_val = self.evaluate_comptime_inst(index, locals, ctx, outer_span)?;
                match base_val {
                    ConstValue::Array(heap_idx) => {
                        let idx = int(index_val, inst_span)?;
                        // Clone elements to release heap borrow before error construction.
                        let elems = match &self.comptime_heap[heap_idx as usize] {
                            ComptimeHeapItem::Array(elems) => elems.clone(),
                            _ => return Err(not_const(inst_span)),
                        };
                        if idx < 0 || idx as usize >= elems.len() {
                            return Err(CompileError::new(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: format!(
                                        "array index {} out of bounds (length {})",
                                        idx,
                                        elems.len()
                                    ),
                                },
                                inst_span,
                            ));
                        }
                        Ok(elems[idx as usize])
                    }
                    _ => Err(not_const(inst_span)),
                }
            }

            // ── Field mutation ────────────────────────────────────────────────
            InstData::FieldSet { base, field, value } => {
                // base must be a VarRef to a local holding a ConstValue::Struct(heap_idx).
                let var_name = match &self.rir.get(base).data {
                    InstData::VarRef { name } => *name,
                    _ => return Err(not_const(inst_span)),
                };
                let heap_idx = match locals.get(&var_name) {
                    Some(ConstValue::Struct(idx)) => *idx,
                    _ => return Err(not_const(inst_span)),
                };
                let val = self.evaluate_comptime_inst(value, locals, ctx, outer_span)?;
                // Resolve field index from struct definition.
                let struct_id = match &self.comptime_heap[heap_idx as usize] {
                    ComptimeHeapItem::Struct { struct_id, .. } => *struct_id,
                    _ => return Err(not_const(inst_span)),
                };
                let struct_def = self.type_pool.struct_def(struct_id);
                let field_name = self.interner.resolve(&field);
                let (field_idx, _) = struct_def.find_field(field_name).ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!("unknown field '{}' in comptime struct", field_name),
                        },
                        inst_span,
                    )
                })?;
                // Mutate the heap item in place.
                match &mut self.comptime_heap[heap_idx as usize] {
                    ComptimeHeapItem::Struct { fields, .. } => {
                        fields[field_idx] = val;
                    }
                    _ => return Err(not_const(inst_span)),
                }
                Ok(ConstValue::Unit)
            }

            // ── Array element mutation ───────────────────────────────────────────
            InstData::IndexSet { base, index, value } => {
                // base must be a VarRef to a local holding a ConstValue::Array(heap_idx).
                let var_name = match &self.rir.get(base).data {
                    InstData::VarRef { name } => *name,
                    _ => return Err(not_const(inst_span)),
                };
                let heap_idx = match locals.get(&var_name) {
                    Some(ConstValue::Array(idx)) => *idx,
                    _ => return Err(not_const(inst_span)),
                };
                let idx = int(
                    self.evaluate_comptime_inst(index, locals, ctx, outer_span)?,
                    inst_span,
                )?;
                let val = self.evaluate_comptime_inst(value, locals, ctx, outer_span)?;
                // Bounds check and mutate.
                let len = match &self.comptime_heap[heap_idx as usize] {
                    ComptimeHeapItem::Array(elems) => elems.len(),
                    _ => return Err(not_const(inst_span)),
                };
                if idx < 0 || idx as usize >= len {
                    return Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!("array index {} out of bounds (length {})", idx, len),
                        },
                        inst_span,
                    ));
                }
                match &mut self.comptime_heap[heap_idx as usize] {
                    ComptimeHeapItem::Array(elems) => {
                        elems[idx as usize] = val;
                    }
                    _ => return Err(not_const(inst_span)),
                }
                Ok(ConstValue::Unit)
            }

            // ── Unit enum variant ──────────────────────────────────────────────
            InstData::EnumVariant {
                module: _,
                type_name,
                variant,
            } => {
                // Resolve enum ID — check direct enums, then comptime type vars.
                let enum_id = if let Some(&id) = self.enums.get(&type_name) {
                    id
                } else if let Some(&ty) = ctx.comptime_type_vars.get(&type_name) {
                    match ty.kind() {
                        TypeKind::Enum(id) => id,
                        _ => return Err(not_const(inst_span)),
                    }
                } else {
                    return Err(not_const(inst_span));
                };
                let enum_def = self.type_pool.enum_def(enum_id);
                let variant_name = self.interner.resolve(&variant);
                let variant_idx = enum_def
                    .find_variant(variant_name)
                    .ok_or_else(|| not_const(inst_span))? as u32;
                Ok(ConstValue::EnumVariant {
                    enum_id,
                    variant_idx,
                })
            }

            // ── Struct-style enum variant ─────────────────────────────────────────
            InstData::EnumStructVariant {
                module: _,
                type_name,
                variant,
                fields_start,
                fields_len,
            } => {
                // Resolve enum ID.
                let enum_id = if let Some(&id) = self.enums.get(&type_name) {
                    id
                } else if let Some(&ty) = ctx.comptime_type_vars.get(&type_name) {
                    match ty.kind() {
                        TypeKind::Enum(id) => id,
                        _ => return Err(not_const(inst_span)),
                    }
                } else {
                    return Err(not_const(inst_span));
                };
                let enum_def = self.type_pool.enum_def(enum_id);
                let variant_name = self.interner.resolve(&variant);
                let variant_idx = enum_def
                    .find_variant(variant_name)
                    .ok_or_else(|| not_const(inst_span))? as u32;
                let variant_def = &enum_def.variants[variant_idx as usize];

                // Get field initializers and resolve to declaration order.
                let field_inits = self.rir.get_field_inits(fields_start, fields_len);
                let mut field_values = vec![ConstValue::Unit; variant_def.fields.len()];
                for (init_field_name, field_value_ref) in &field_inits {
                    let field_name_str = self.interner.resolve(init_field_name);
                    let field_idx = variant_def
                        .find_field(field_name_str)
                        .ok_or_else(|| not_const(inst_span))?;
                    let val =
                        self.evaluate_comptime_inst(*field_value_ref, locals, ctx, outer_span)?;
                    field_values[field_idx] = val;
                }

                let heap_idx = self.comptime_heap.len() as u32;
                self.comptime_heap
                    .push(ComptimeHeapItem::EnumStruct(field_values));
                Ok(ConstValue::EnumStruct {
                    enum_id,
                    variant_idx,
                    heap_idx,
                })
            }

            // ── Tuple data enum variant (via AssocFnCall) ─────────────────────────
            InstData::AssocFnCall {
                type_name,
                function,
                args_start,
                args_len,
            } => {
                // Check if this is an enum data variant construction.
                let enum_id = if let Some(&id) = self.enums.get(&type_name) {
                    Some(id)
                } else if let Some(&ty) = ctx.comptime_type_vars.get(&type_name) {
                    match ty.kind() {
                        TypeKind::Enum(id) => Some(id),
                        _ => None,
                    }
                } else {
                    None
                };

                if let Some(enum_id) = enum_id {
                    let enum_def = self.type_pool.enum_def(enum_id);
                    let variant_name = self.interner.resolve(&function);
                    if let Some(variant_idx) = enum_def.find_variant(variant_name) {
                        let variant_def = &enum_def.variants[variant_idx];
                        if variant_def.has_data() && !variant_def.is_struct_variant() {
                            // Tuple data variant: evaluate arguments.
                            let call_args = self.rir.get_call_args(args_start, args_len);
                            let mut field_values = Vec::with_capacity(variant_def.fields.len());
                            for arg in &call_args {
                                let val = self
                                    .evaluate_comptime_inst(arg.value, locals, ctx, outer_span)?;
                                field_values.push(val);
                            }
                            let heap_idx = self.comptime_heap.len() as u32;
                            self.comptime_heap
                                .push(ComptimeHeapItem::EnumData(field_values));
                            return Ok(ConstValue::EnumData {
                                enum_id,
                                variant_idx: variant_idx as u32,
                                heap_idx,
                            });
                        }
                    }
                }

                // Not an enum data variant — unsupported in comptime.
                Err(not_const(inst_span))
            }

            // ── Pattern matching ───────────────────────────────────────────────
            InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            } => {
                let scrut_val = self.evaluate_comptime_inst(scrutinee, locals, ctx, outer_span)?;
                let arms = self.rir.get_match_arms(arms_start, arms_len);

                for (pattern, body) in &arms {
                    match pattern {
                        RirPattern::Wildcard(_) => {
                            // Always matches — evaluate body directly.
                            return self.evaluate_comptime_inst(*body, locals, ctx, outer_span);
                        }
                        RirPattern::Int(n, _) => {
                            if let ConstValue::Integer(val) = scrut_val
                                && val == *n
                            {
                                return self.evaluate_comptime_inst(*body, locals, ctx, outer_span);
                            }
                        }
                        RirPattern::Bool(b, _) => {
                            if let ConstValue::Bool(val) = scrut_val
                                && val == *b
                            {
                                return self.evaluate_comptime_inst(*body, locals, ctx, outer_span);
                            }
                        }
                        RirPattern::Path {
                            type_name, variant, ..
                        } => {
                            // Match unit enum variant by name.
                            let pat_enum_id = self.resolve_comptime_enum(*type_name, ctx);
                            if let Some(pat_enum_id) = pat_enum_id {
                                let enum_def = self.type_pool.enum_def(pat_enum_id);
                                let variant_name = self.interner.resolve(variant);
                                if let Some(pat_variant_idx) = enum_def.find_variant(variant_name) {
                                    let matches = match scrut_val {
                                        ConstValue::EnumVariant {
                                            enum_id,
                                            variant_idx,
                                        } => {
                                            enum_id == pat_enum_id
                                                && variant_idx == pat_variant_idx as u32
                                        }
                                        ConstValue::EnumData {
                                            enum_id,
                                            variant_idx,
                                            ..
                                        } => {
                                            enum_id == pat_enum_id
                                                && variant_idx == pat_variant_idx as u32
                                        }
                                        ConstValue::EnumStruct {
                                            enum_id,
                                            variant_idx,
                                            ..
                                        } => {
                                            enum_id == pat_enum_id
                                                && variant_idx == pat_variant_idx as u32
                                        }
                                        _ => false,
                                    };
                                    if matches {
                                        return self.evaluate_comptime_inst(
                                            *body, locals, ctx, outer_span,
                                        );
                                    }
                                }
                            }
                        }
                        RirPattern::DataVariant {
                            type_name,
                            variant,
                            bindings,
                            ..
                        } => {
                            // Match tuple data variant and bind fields.
                            let pat_enum_id = self.resolve_comptime_enum(*type_name, ctx);
                            if let Some(pat_enum_id) = pat_enum_id {
                                let enum_def = self.type_pool.enum_def(pat_enum_id);
                                let variant_name = self.interner.resolve(variant);
                                if let Some(pat_variant_idx) = enum_def.find_variant(variant_name) {
                                    let (matches, heap_idx_opt) = match scrut_val {
                                        ConstValue::EnumData {
                                            enum_id,
                                            variant_idx,
                                            heap_idx,
                                        } if enum_id == pat_enum_id
                                            && variant_idx == pat_variant_idx as u32 =>
                                        {
                                            (true, Some(heap_idx))
                                        }
                                        _ => (false, None),
                                    };
                                    if matches {
                                        // Bind fields into locals.
                                        if let Some(heap_idx) = heap_idx_opt {
                                            let field_values =
                                                match &self.comptime_heap[heap_idx as usize] {
                                                    ComptimeHeapItem::EnumData(fields) => {
                                                        fields.clone()
                                                    }
                                                    _ => return Err(not_const(inst_span)),
                                                };
                                            for (i, binding) in bindings.iter().enumerate() {
                                                if !binding.is_wildcard
                                                    && let Some(name) = binding.name
                                                {
                                                    locals.insert(name, field_values[i]);
                                                }
                                            }
                                        }
                                        return self.evaluate_comptime_inst(
                                            *body, locals, ctx, outer_span,
                                        );
                                    }
                                }
                            }
                        }
                        RirPattern::StructVariant {
                            type_name,
                            variant,
                            field_bindings,
                            ..
                        } => {
                            // Match struct variant and bind named fields.
                            let pat_enum_id = self.resolve_comptime_enum(*type_name, ctx);
                            if let Some(pat_enum_id) = pat_enum_id {
                                let enum_def = self.type_pool.enum_def(pat_enum_id);
                                let variant_name = self.interner.resolve(variant);
                                if let Some(pat_variant_idx) = enum_def.find_variant(variant_name) {
                                    let (matches, heap_idx_opt) = match scrut_val {
                                        ConstValue::EnumStruct {
                                            enum_id,
                                            variant_idx,
                                            heap_idx,
                                        } if enum_id == pat_enum_id
                                            && variant_idx == pat_variant_idx as u32 =>
                                        {
                                            (true, Some(heap_idx))
                                        }
                                        _ => (false, None),
                                    };
                                    if matches {
                                        if let Some(heap_idx) = heap_idx_opt {
                                            let field_values =
                                                match &self.comptime_heap[heap_idx as usize] {
                                                    ComptimeHeapItem::EnumStruct(fields) => {
                                                        fields.clone()
                                                    }
                                                    _ => return Err(not_const(inst_span)),
                                                };
                                            let variant_def = &enum_def.variants[pat_variant_idx];
                                            for fb in field_bindings {
                                                if !fb.binding.is_wildcard
                                                    && let Some(name) = fb.binding.name
                                                {
                                                    let field_name_str =
                                                        self.interner.resolve(&fb.field_name);
                                                    let field_idx = match variant_def
                                                        .find_field(field_name_str)
                                                    {
                                                        Some(idx) => idx,
                                                        None => return Err(not_const(inst_span)),
                                                    };
                                                    locals.insert(name, field_values[field_idx]);
                                                }
                                            }
                                        }
                                        return self.evaluate_comptime_inst(
                                            *body, locals, ctx, outer_span,
                                        );
                                    }
                                }
                            }
                        }
                        // ADR-0051 Phase 4a: Ident / Tuple / Struct are not yet
                        // produced by astgen, so comptime match evaluation does
                        // not need to handle them yet. Phase 4b turns astgen on
                        // and fills these in.
                        RirPattern::Ident { .. }
                        | RirPattern::Tuple { .. }
                        | RirPattern::Struct { .. } => {
                            unreachable!(
                                "RirPattern::Ident/Tuple/Struct are not produced by astgen in \
                                 ADR-0051 Phase 4a"
                            )
                        }
                    }
                }

                // No arm matched — should not happen after exhaustiveness checking.
                Err(CompileError::new(
                    ErrorKind::ComptimeEvaluationFailed {
                        reason: "no match arm matched in comptime evaluation".into(),
                    },
                    inst_span,
                ))
            }

            // ── Struct destructuring ─────────────────────────────────────────
            InstData::StructDestructure {
                type_name,
                fields_start,
                fields_len,
                init,
            } => {
                // Evaluate the initializer to a struct value.
                let init_val = self.evaluate_comptime_inst(init, locals, ctx, outer_span)?;
                let heap_idx = match init_val {
                    ConstValue::Struct(idx) => idx,
                    _ => return Err(not_const(inst_span)),
                };

                // Resolve the struct type.
                let struct_id = match self.resolve_comptime_struct(type_name, ctx) {
                    Some(id) => id,
                    None => return Err(not_const(inst_span)),
                };

                // Get field values from the heap.
                let field_values = match &self.comptime_heap[heap_idx as usize] {
                    ComptimeHeapItem::Struct { fields, .. } => fields.clone(),
                    _ => return Err(not_const(inst_span)),
                };

                // Get the struct definition for field name lookup.
                let struct_def = self.type_pool.struct_def(struct_id);
                let field_name_to_idx: HashMap<String, usize> = struct_def
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, f)| (f.name.clone(), i))
                    .collect();

                // Bind each field into locals.
                let destr_fields = self.rir.get_destructure_fields(fields_start, fields_len);
                for field in &destr_fields {
                    if field.is_wildcard {
                        continue;
                    }
                    let field_name = self.interner.resolve(&field.field_name).to_string();
                    let field_idx = match field_name_to_idx.get(&field_name) {
                        Some(&idx) => idx,
                        None => return Err(not_const(inst_span)),
                    };
                    let binding_name = field.binding_name.unwrap_or(field.field_name);
                    locals.insert(binding_name, field_values[field_idx]);
                }
                Ok(ConstValue::Unit)
            }

            // ── Method call ─────────────────────────────────────────────────────
            InstData::MethodCall {
                receiver,
                method,
                args_start,
                args_len,
            } => {
                const COMPTIME_CALL_DEPTH_LIMIT: u32 = 64;

                // Evaluate the receiver to get the value.
                let receiver_val =
                    self.evaluate_comptime_inst(receiver, locals, ctx, outer_span)?;

                // Handle comptime_str method dispatch.
                if let ConstValue::ComptimeStr(str_idx) = receiver_val {
                    let method_name = self.interner.resolve(&method).to_string();
                    let call_args = self.rir.get_call_args(args_start, args_len);
                    return self.evaluate_comptime_str_method(
                        str_idx,
                        &method_name,
                        &call_args,
                        locals,
                        ctx,
                        ComptimeSpans {
                            outer: outer_span,
                            inst: inst_span,
                        },
                    );
                }

                // Determine the struct type from the receiver value.
                let struct_id = match receiver_val {
                    ConstValue::Struct(heap_idx) => match &self.comptime_heap[heap_idx as usize] {
                        ComptimeHeapItem::Struct { struct_id, .. } => *struct_id,
                        _ => return Err(not_const(inst_span)),
                    },
                    _ => return Err(not_const(inst_span)),
                };

                // Look up the method.
                let method_key = (struct_id, method);
                let method_info = match self.methods.get(&method_key) {
                    Some(info) => *info,
                    None => return Err(not_const(inst_span)),
                };

                // Evaluate all explicit arguments.
                let call_args = self.rir.get_call_args(args_start, args_len);
                let mut arg_values = Vec::with_capacity(call_args.len());
                for call_arg in &call_args {
                    let val =
                        self.evaluate_comptime_inst(call_arg.value, locals, ctx, outer_span)?;
                    arg_values.push(val);
                }

                // Enforce call stack depth limit.
                if self.comptime_call_depth >= COMPTIME_CALL_DEPTH_LIMIT {
                    return Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "comptime call stack depth exceeded {} levels",
                                COMPTIME_CALL_DEPTH_LIMIT
                            ),
                        },
                        inst_span,
                    ));
                }

                // Bind self and parameters.
                let param_names = self.param_arena.names(method_info.params).to_vec();
                let mut call_locals: HashMap<Spur, ConstValue> = {
                    let mut m = HashMap::default();
                    m.reserve(param_names.len() + 1);
                    m
                };
                // Bind `self`.
                let self_sym = self.interner.get_or_intern("self");
                call_locals.insert(self_sym, receiver_val);
                for (param_name, arg_val) in param_names.iter().zip(arg_values.iter()) {
                    call_locals.insert(*param_name, *arg_val);
                }

                // Execute the method body.
                self.comptime_call_depth += 1;
                let body_result = self.evaluate_comptime_inst(
                    method_info.body,
                    &mut call_locals,
                    ctx,
                    outer_span,
                );
                self.comptime_call_depth -= 1;
                let body_result = body_result?;

                match body_result {
                    ConstValue::ReturnSignal => {
                        self.comptime_return_value.take().ok_or_else(|| {
                            CompileError::new(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: "comptime return signal missing its value".into(),
                                },
                                inst_span,
                            )
                        })
                    }
                    ConstValue::BreakSignal | ConstValue::ContinueSignal => Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: "break/continue outside a loop in comptime method".into(),
                        },
                        inst_span,
                    )),
                    val => Ok(val),
                }
            }

            // ── Intrinsic ────────────────────────────────────────────────────────
            InstData::Intrinsic {
                name,
                args_start,
                args_len,
            } => {
                // @cast is a no-op in comptime since all integers are i64.
                if name == self.known.cast {
                    let arg_refs = self.rir.get_inst_refs(args_start, args_len);
                    if arg_refs.len() != 1 {
                        return Err(not_const(inst_span));
                    }
                    return self.evaluate_comptime_inst(arg_refs[0], locals, ctx, outer_span);
                }
                // @dbg formats values, prints to stderr on-the-fly (unless
                // suppressed), appends to the comptime dbg buffer, and queues a
                // warning to be emitted after sema completes.
                if name == self.known.dbg {
                    let arg_refs = self.rir.get_inst_refs(args_start, args_len);
                    let mut parts = Vec::with_capacity(arg_refs.len());
                    for &arg_ref in &arg_refs {
                        let val = self.evaluate_comptime_inst(arg_ref, locals, ctx, outer_span)?;
                        parts.push(self.format_const_value(val, inst_span)?);
                    }
                    let msg = parts.join(" ");
                    if !self.suppress_comptime_dbg_print {
                        eprintln!("comptime dbg: {msg}");
                    }
                    self.comptime_dbg_output.push(msg.clone());
                    self.comptime_log_output.push((msg, inst_span));
                    return Ok(ConstValue::Unit);
                }
                // @compile_error emits a user-defined compile error.
                if name == self.known.compile_error {
                    let arg_refs = self.rir.get_inst_refs(args_start, args_len);
                    if arg_refs.len() != 1 {
                        return Err(CompileError::new(
                            ErrorKind::IntrinsicWrongArgCount {
                                name: "compile_error".to_string(),
                                expected: 1,
                                found: arg_refs.len(),
                            },
                            inst_span,
                        ));
                    }
                    let msg =
                        self.evaluate_comptime_string_arg(arg_refs[0], locals, ctx, outer_span)?;
                    return Err(CompileError::new(
                        ErrorKind::ComptimeUserError(msg),
                        inst_span,
                    ));
                }
                // @range produces a comptime array of integers.
                if name == self.known.range {
                    let arg_refs = self.rir.get_inst_refs(args_start, args_len);
                    let (start, end, stride) = match arg_refs.len() {
                        1 => {
                            let end = int(
                                self.evaluate_comptime_inst(arg_refs[0], locals, ctx, outer_span)?,
                                inst_span,
                            )?;
                            (0i64, end, 1i64)
                        }
                        2 => {
                            let s = int(
                                self.evaluate_comptime_inst(arg_refs[0], locals, ctx, outer_span)?,
                                inst_span,
                            )?;
                            let e = int(
                                self.evaluate_comptime_inst(arg_refs[1], locals, ctx, outer_span)?,
                                inst_span,
                            )?;
                            (s, e, 1i64)
                        }
                        3 => {
                            let s = int(
                                self.evaluate_comptime_inst(arg_refs[0], locals, ctx, outer_span)?,
                                inst_span,
                            )?;
                            let e = int(
                                self.evaluate_comptime_inst(arg_refs[1], locals, ctx, outer_span)?,
                                inst_span,
                            )?;
                            let st = int(
                                self.evaluate_comptime_inst(arg_refs[2], locals, ctx, outer_span)?,
                                inst_span,
                            )?;
                            (s, e, st)
                        }
                        _ => {
                            return Err(CompileError::new(
                                ErrorKind::IntrinsicWrongArgCount {
                                    name: "range".to_string(),
                                    expected: 1,
                                    found: arg_refs.len(),
                                },
                                inst_span,
                            ));
                        }
                    };
                    if stride == 0 {
                        return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: "@range stride must not be zero".into(),
                            },
                            inst_span,
                        ));
                    }
                    // Cap the element count to prevent OOM from e.g. @range(i64::MAX)
                    const MAX_RANGE_ELEMENTS: usize = 1_000_000;
                    let mut elements = Vec::new();
                    let mut i = start;
                    if stride > 0 {
                        while i < end {
                            if elements.len() >= MAX_RANGE_ELEMENTS {
                                return Err(CompileError::new(
                                    ErrorKind::ComptimeEvaluationFailed {
                                        reason: format!(
                                            "@range produces too many elements (limit is {})",
                                            MAX_RANGE_ELEMENTS
                                        ),
                                    },
                                    inst_span,
                                ));
                            }
                            elements.push(ConstValue::Integer(i));
                            i = i.checked_add(stride).ok_or_else(|| overflow(inst_span))?;
                        }
                    } else {
                        while i > end {
                            if elements.len() >= MAX_RANGE_ELEMENTS {
                                return Err(CompileError::new(
                                    ErrorKind::ComptimeEvaluationFailed {
                                        reason: format!(
                                            "@range produces too many elements (limit is {})",
                                            MAX_RANGE_ELEMENTS
                                        ),
                                    },
                                    inst_span,
                                ));
                            }
                            elements.push(ConstValue::Integer(i));
                            i = i.checked_add(stride).ok_or_else(|| overflow(inst_span))?;
                        }
                    }
                    let idx = self.comptime_heap.len() as u32;
                    self.comptime_heap.push(ComptimeHeapItem::Array(elements));
                    return Ok(ConstValue::Array(idx));
                }
                // Platform intrinsics return variants of the built-in Os/Arch enums.
                // They are pure functions of the compile target, so the comptime
                // interpreter can evaluate them directly.
                if let Some(id) = self.known.intrinsic_id(name) {
                    match id {
                        IntrinsicId::TargetOs => {
                            let enum_id = self
                                .builtin_os_id
                                .expect("Os enum not injected - internal compiler error");
                            let variant_idx = match gruel_target::Target::host().os() {
                                gruel_target::Os::Linux => 0,
                                gruel_target::Os::Macos => 1,
                            };
                            return Ok(ConstValue::EnumVariant {
                                enum_id,
                                variant_idx,
                            });
                        }
                        IntrinsicId::TargetArch => {
                            let enum_id = self
                                .builtin_arch_id
                                .expect("Arch enum not injected - internal compiler error");
                            let variant_idx = match gruel_target::Target::host().arch() {
                                gruel_target::Arch::X86_64 => 0,
                                gruel_target::Arch::Aarch64 => 1,
                            };
                            return Ok(ConstValue::EnumVariant {
                                enum_id,
                                variant_idx,
                            });
                        }
                        _ => {}
                    }
                }
                // Unrecognized intrinsic: surface the name in the diagnostic
                // rather than the generic "cannot be known at compile time"
                // message. In particular, `@compileLog` was removed in favor of
                // `@dbg` — reporting the name guides users to the replacement.
                let intrinsic_name = self.interner.resolve(&name).to_string();
                Err(CompileError::new(
                    ErrorKind::UnknownIntrinsic(intrinsic_name),
                    inst_span,
                ))
            }

            // ── Type intrinsic (@size_of, @align_of, @type_name, @type_info) ──────
            InstData::TypeIntrinsic { name, type_arg } => {
                // Resolve the type argument.
                // Check comptime_type_overrides first (for generic type params like T),
                // then comptime_type_vars from the analysis context, then fall back
                // to the normal type resolver.
                let ty = if let Some(&override_ty) = self.comptime_type_overrides.get(&type_arg) {
                    override_ty
                } else if let Some(&ctx_ty) = ctx.comptime_type_vars.get(&type_arg) {
                    ctx_ty
                } else if let Some(&var_ty) = locals.iter().find_map(|(k, v)| {
                    if *k == type_arg {
                        if let ConstValue::Type(t) = v {
                            Some(t)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }) {
                    var_ty
                } else {
                    self.resolve_type(type_arg, inst_span)
                        .map_err(|_| not_const(inst_span))?
                };
                match self.known.intrinsic_id(name) {
                    Some(IntrinsicId::SizeOf) => {
                        let slot_count = self.abi_slot_count(ty);
                        Ok(ConstValue::Integer((slot_count as i64) * 8))
                    }
                    Some(IntrinsicId::AlignOf) => {
                        let slot_count = self.abi_slot_count(ty);
                        Ok(ConstValue::Integer(if slot_count == 0 { 1 } else { 8 }))
                    }
                    Some(IntrinsicId::TypeName) => self.evaluate_comptime_type_name(ty, inst_span),
                    Some(IntrinsicId::TypeInfo) => self.evaluate_comptime_type_info(ty, inst_span),
                    Some(IntrinsicId::Ownership) => {
                        let enum_id = self
                            .builtin_ownership_id
                            .expect("Ownership enum not injected - internal compiler error");
                        let variant_idx = self.ownership_variant_index(ty);
                        Ok(ConstValue::EnumVariant {
                            enum_id,
                            variant_idx,
                        })
                    }
                    _ => Err(not_const(inst_span)),
                }
            }

            // ── Type+interface intrinsic (@conforms) ───────────────────────────
            InstData::TypeInterfaceIntrinsic {
                name,
                type_arg,
                interface_arg,
            } => {
                let ty = if let Some(&override_ty) = self.comptime_type_overrides.get(&type_arg) {
                    override_ty
                } else if let Some(&ctx_ty) = ctx.comptime_type_vars.get(&type_arg) {
                    ctx_ty
                } else {
                    self.resolve_type(type_arg, inst_span)
                        .map_err(|_| not_const(inst_span))?
                };
                match self.known.intrinsic_id(name) {
                    Some(IntrinsicId::Conforms) => {
                        let interface_id = self
                            .interfaces
                            .get(&interface_arg)
                            .copied()
                            .ok_or_else(|| not_const(inst_span))?;
                        let value = self.check_conforms(ty, interface_id, inst_span).is_ok();
                        Ok(ConstValue::Bool(value))
                    }
                    _ => Err(not_const(inst_span)),
                }
            }

            // ── Not yet supported ─────────────────────────────────────────────
            _ => Err(not_const(inst_span)),
        }
    }

    /// Check if an RIR instruction is a VarRef to a comptime type variable.
    ///
    /// This is used when validating comptime arguments to detect variables
    /// that hold comptime type values (e.g., `let P = Point(); ... Line(P)`).
    pub(crate) fn is_comptime_type_var(&self, inst_ref: InstRef, ctx: &AnalysisContext) -> bool {
        if let InstData::VarRef { name } = &self.rir.get(inst_ref).data {
            ctx.comptime_type_vars.contains_key(name)
        } else {
            false
        }
    }

    /// Resolve an enum type by name during comptime evaluation.
    /// Checks direct enums first, then comptime type variables.
    fn resolve_comptime_enum(&self, type_name: Spur, ctx: &AnalysisContext) -> Option<EnumId> {
        if let Some(&id) = self.enums.get(&type_name) {
            Some(id)
        } else if let Some(&ty) = self.comptime_type_overrides.get(&type_name) {
            match ty.kind() {
                TypeKind::Enum(id) => Some(id),
                _ => None,
            }
        } else if let Some(&ty) = ctx.comptime_type_vars.get(&type_name) {
            match ty.kind() {
                TypeKind::Enum(id) => Some(id),
                _ => None,
            }
        } else {
            None
        }
    }

    /// Resolve a struct type by name during comptime evaluation.
    /// Checks direct structs first, then comptime type overrides, then comptime type variables.
    fn resolve_comptime_struct(&self, type_name: Spur, ctx: &AnalysisContext) -> Option<StructId> {
        if let Some(&id) = self.structs.get(&type_name) {
            Some(id)
        } else if let Some(&ty) = self.comptime_type_overrides.get(&type_name) {
            match ty.kind() {
                TypeKind::Struct(id) => Some(id),
                _ => None,
            }
        } else if let Some(&ty) = ctx.comptime_type_vars.get(&type_name) {
            match ty.kind() {
                TypeKind::Struct(id) => Some(id),
                _ => None,
            }
        } else {
            None
        }
    }

    /// Check if an RIR instruction is a comparison operation.
    ///
    /// This is used to detect chained comparisons (e.g., `a < b < c`) which are
    /// not allowed in Gruel.
    fn is_comparison(&self, inst_ref: InstRef) -> bool {
        matches!(
            self.rir.get(inst_ref).data,
            InstData::Bin {
                op: BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge | BinOp::Eq | BinOp::Ne,
                ..
            }
        )
    }

    /// Analyze a builtin type associated function call.
    ///
    /// Dispatches to the appropriate runtime function based on the builtin registry.
    fn analyze_builtin_assoc_fn(
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
                BuiltinParamType::SelfType => Type::new_struct(struct_id),
            };

            // Type check
            if arg_result.ty != expected_ty && !arg_result.ty.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: expected_ty.name().to_string(),
                        found: arg_result.ty.name().to_string(),
                    },
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
    fn analyze_builtin_method(
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

        // Analyze arguments and check types
        let mut air_args: Vec<(AirRef, AirArgMode)> = Vec::with_capacity(args.len() + 1);

        // Add receiver as first argument
        air_args.push((receiver.result.air_ref, AirArgMode::Normal));

        // Analyze and add other arguments
        for (i, arg) in args.iter().enumerate() {
            let arg_result = self.analyze_inst(air, arg.value, ctx)?;

            // Get expected type from param
            let expected_ty = match method.params[i].ty {
                BuiltinParamType::U64 => Type::U64,
                BuiltinParamType::Usize => Type::USIZE,
                BuiltinParamType::U8 => Type::U8,
                BuiltinParamType::Bool => Type::BOOL,
                BuiltinParamType::SelfType => Type::new_struct(method_ctx.struct_id),
            };

            // Type check
            if arg_result.ty != expected_ty
                && !arg_result.ty.is_error()
                && !(self.is_builtin_string(arg_result.ty)
                    && matches!(method.params[i].ty, BuiltinParamType::SelfType))
            {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: expected_ty.name().to_string(),
                        found: arg_result.ty.name().to_string(),
                    },
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

        // For mutation methods, store the result back to the receiver
        if method.receiver_mode == ReceiverMode::ByMutRef {
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
    fn get_string_receiver_storage(
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
                        RirParamMode::Inout => {
                            return Ok(Some(StringReceiverStorage::Param {
                                abi_slot: param_info.abi_slot,
                            }));
                        }
                        RirParamMode::Borrow => {
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

    /// Check if directives contain @allow for a specific warning name.
    pub(crate) fn has_allow_directive(
        &self,
        directives: &[RirDirective],
        warning_name: &str,
    ) -> bool {
        let allow_sym = self.interner.get("allow");
        let warning_sym = self.interner.get(warning_name);

        for directive in directives {
            if Some(directive.name) == allow_sym {
                for arg in &directive.args {
                    if Some(*arg) == warning_sym {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Check for unused local variables in the current scope (before popping it).
    /// Uses the scope stack to determine which variables were added in the current scope.
    pub(crate) fn check_unused_locals_in_current_scope(&self, ctx: &mut AnalysisContext) {
        // Get the current scope entries (variables added in this scope)
        let Some(current_scope) = ctx.scope_stack.last() else {
            return;
        };

        for (symbol, _old_value) in current_scope {
            // Skip if variable was used
            if ctx.used_locals.contains(symbol) {
                continue;
            }

            // Get the local var info (it should still be in ctx.locals before pop)
            let Some(local) = ctx.locals.get(symbol) else {
                continue;
            };

            // Get variable name
            let name = self.interner.resolve(symbol);

            // Skip variables starting with underscore (convention for intentionally unused)
            if name.starts_with('_') {
                continue;
            }

            // Skip if @allow(unused_variable) was applied
            if local.allow_unused {
                continue;
            }

            // Emit warning with help suggestion (to ctx.warnings for parallel safety)
            ctx.warnings.push(
                CompileWarning::new(WarningKind::UnusedVariable(name.to_string()), local.span)
                    .with_help(format!(
                        "if this is intentional, prefix it with an underscore: `_{}`",
                        name
                    )),
            );
        }
    }

    /// Check for unconsumed linear values in the current scope (before popping it).
    /// Linear values MUST be consumed (moved) - it's an error to let them drop implicitly.
    /// Returns an error if any linear value was not consumed.
    pub(crate) fn check_unconsumed_linear_values(
        &self,
        ctx: &AnalysisContext,
    ) -> CompileResult<()> {
        // Get the current scope entries (variables added in this scope)
        let Some(current_scope) = ctx.scope_stack.last() else {
            return Ok(());
        };

        for (symbol, _old_value) in current_scope {
            // Get the local var info (it should still be in ctx.locals before pop)
            let Some(local) = ctx.locals.get(symbol) else {
                continue;
            };

            // Only check linear types
            if !self.is_type_linear(local.ty) {
                continue;
            }

            // Check if this variable was moved (consumed)
            let was_consumed = ctx
                .moved_vars
                .get(symbol)
                .is_some_and(|state| state.full_move.is_some());

            if !was_consumed {
                let name = self.interner.resolve(symbol);
                return Err(CompileError::new(
                    ErrorKind::LinearValueNotConsumed(name.to_string()),
                    local.span,
                ));
            }
        }

        Ok(())
    }

    /// Extract the root variable symbol from an expression, if it refers to a variable.
    ///
    /// For inout arguments, we need to track which variable is being passed to detect
    /// when the same variable is passed to multiple inout parameters.
    ///
    /// Returns Some(symbol) for:
    /// - VarRef { name } -> the variable symbol
    /// - ParamRef { name, .. } -> the parameter symbol
    /// - FieldGet { base, .. } -> recursively extract from base
    /// - IndexGet { base, .. } -> recursively extract from base
    ///
    /// Returns None for expressions that don't refer to a variable (literals, calls, etc.)
    pub(crate) fn extract_root_variable(&self, inst_ref: InstRef) -> Option<Spur> {
        let inst = self.rir.get(inst_ref);
        match &inst.data {
            InstData::VarRef { name } => Some(*name),
            InstData::ParamRef { name, .. } => Some(*name),
            InstData::FieldGet { base, .. } => self.extract_root_variable(*base),
            InstData::IndexGet { base, .. } => self.extract_root_variable(*base),
            _ => None,
        }
    }

    /// Check exclusivity rules for inout and borrow parameters in a call.
    ///
    /// This enforces two rules:
    /// 1. Same variable cannot be passed to multiple inout parameters (prevents aliasing)
    /// 2. Same variable cannot be passed to both inout and borrow (law of exclusivity)
    ///
    /// The law of exclusivity: either one mutable (inout) access OR any number of
    /// immutable (borrow) accesses, never both simultaneously.
    pub(crate) fn check_exclusive_access(
        &self,
        args: &[RirCallArg],
        call_span: Span,
    ) -> CompileResult<()> {
        use rustc_hash::FxHashSet as HashSet;
        let mut inout_vars: HashSet<Spur> = HashSet::default();
        let mut borrow_vars: HashSet<Spur> = HashSet::default();

        for arg in args {
            // Classify the borrow/inout shape of this arg, considering both
            // the legacy `borrow`/`inout` modes (ADR-0013) and the new `&x` /
            // `&mut x` MakeRef expressions (ADR-0062). Both produce the same
            // exclusivity obligations.
            let (is_inout_like, is_borrow_like) = self.classify_borrowing_arg(arg);

            // Lvalue check for legacy modes is here; for MakeRef the lvalue
            // check happens in `analyze_make_ref`.
            if arg.is_inout() {
                if self.extract_root_variable(arg.value).is_none() {
                    return Err(CompileError::new(
                        ErrorKind::InoutNonLvalue,
                        self.rir.get(arg.value).span,
                    ));
                }
            } else if arg.is_borrow() && self.extract_root_variable(arg.value).is_none() {
                return Err(CompileError::new(
                    ErrorKind::BorrowNonLvalue,
                    self.rir.get(arg.value).span,
                ));
            }

            let maybe_var_symbol = self.extract_borrowing_root_variable(arg.value);

            if let Some(var_symbol) = maybe_var_symbol {
                if is_inout_like {
                    // Check for duplicate inout access
                    if !inout_vars.insert(var_symbol) {
                        let var_name = self.interner.resolve(&var_symbol).to_string();
                        return Err(CompileError::new(
                            ErrorKind::InoutExclusiveAccess { variable: var_name },
                            call_span,
                        ));
                    }
                    // Check for borrow/inout conflict
                    if borrow_vars.contains(&var_symbol) {
                        let var_name = self.interner.resolve(&var_symbol).to_string();
                        return Err(CompileError::new(
                            ErrorKind::BorrowInoutConflict { variable: var_name },
                            call_span,
                        ));
                    }
                } else if is_borrow_like {
                    borrow_vars.insert(var_symbol);
                    // Check for borrow/inout conflict
                    if inout_vars.contains(&var_symbol) {
                        let var_name = self.interner.resolve(&var_symbol).to_string();
                        return Err(CompileError::new(
                            ErrorKind::BorrowInoutConflict { variable: var_name },
                            call_span,
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Classify a call argument's borrowing shape (ADR-0013 + ADR-0062).
    ///
    /// Returns `(is_inout_like, is_borrow_like)`. `inout`/`&mut x` count as
    /// inout-like; `borrow`/`&x` count as borrow-like. Normal-mode arguments
    /// where the value is not a MakeRef return `(false, false)`.
    pub(crate) fn classify_borrowing_arg(&self, arg: &RirCallArg) -> (bool, bool) {
        if arg.is_inout() {
            return (true, false);
        }
        if arg.is_borrow() {
            return (false, true);
        }
        if let InstData::MakeRef { is_mut, .. } = self.rir.get(arg.value).data {
            return (is_mut, !is_mut);
        }
        (false, false)
    }

    /// Like `extract_root_variable`, but transparently descends through a
    /// `MakeRef` wrapper so that `&x` / `&mut x` resolve to the same root as
    /// `borrow x` / `inout x`.
    pub(crate) fn extract_borrowing_root_variable(&self, inst_ref: InstRef) -> Option<Spur> {
        if let InstData::MakeRef { operand, .. } = &self.rir.get(inst_ref).data {
            return self.extract_root_variable(*operand);
        }
        self.extract_root_variable(inst_ref)
    }

    /// Analyze a list of call arguments, handling inout unmove logic.
    ///
    /// For inout arguments, the variable is "unmoving" after analysis - this is because
    /// inout is a mutable borrow, not a move. The value stays valid after the call.
    pub(crate) fn analyze_call_args(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Vec<AirCallArg>> {
        let mut air_args = Vec::new();
        for arg in args.iter() {
            // For inout/borrow arguments and `&x` / `&mut x` constructions
            // (ADR-0062), extract the underlying variable name so we can
            // "unmove" it after analysis — these are borrows, not moves.
            let (is_inout_like, is_borrow_like) = self.classify_borrowing_arg(arg);
            let borrowed_var = if is_inout_like || is_borrow_like {
                self.extract_borrowing_root_variable(arg.value)
            } else {
                None
            };

            let arg_result = self.analyze_inst(air, arg.value, ctx)?;

            // If this was an inout/borrow/ref argument, the variable shouldn't
            // be marked as moved — the value stays valid after the call.
            if let Some(var_symbol) = borrowed_var {
                ctx.moved_vars.remove(&var_symbol);
            }

            air_args.push(AirCallArg {
                value: arg_result.air_ref,
                mode: AirArgMode::from(arg.mode),
            });
        }
        Ok(air_args)
    }

    /// Register methods from an anonymous struct type with type substitution (comptime-safe).
    ///
    /// This variant supports comptime parameter capture by using `resolve_type_for_comptime_with_subst`
    /// to resolve type parameters like `T` to their concrete types from the enclosing function's
    /// comptime arguments.
    fn register_anon_struct_methods_for_comptime_with_subst(
        &mut self,
        spec: AnonStructSpec,
        _span: Span,
        type_subst: &rustc_hash::FxHashMap<Spur, Type>,
        _value_subst: &rustc_hash::FxHashMap<Spur, ConstValue>,
    ) -> Option<()> {
        let AnonStructSpec {
            struct_id,
            struct_type,
            methods_start,
            methods_len,
        } = spec;
        let method_refs = self.rir.get_inst_refs(methods_start, methods_len);

        let mut seen_methods: rustc_hash::FxHashSet<Spur> = rustc_hash::FxHashSet::default();

        for method_ref in method_refs {
            let method_inst = self.rir.get(method_ref);
            if let InstData::FnDecl {
                name: method_name,
                is_unchecked,
                params_start,
                params_len,
                return_type,
                body,
                has_self,
                receiver_mode,
                ..
            } = &method_inst.data
            {
                let receiver = crate::sema::anon_interfaces::decode_receiver_mode(*receiver_mode);
                let key = (struct_id, *method_name);

                if seen_methods.contains(method_name) {
                    return None;
                }
                seen_methods.insert(*method_name);

                if self.methods.contains_key(&key) {
                    return None;
                }

                let params = self.rir.get_params(*params_start, *params_len);
                let param_names: Vec<Spur> = params.iter().map(|p| p.name).collect();
                let param_modes: Vec<RirParamMode> = params.iter().map(|p| p.mode).collect();
                let param_comptime: Vec<bool> = params.iter().map(|p| p.is_comptime).collect();
                let mut param_types: Vec<Type> = Vec::with_capacity(params.len());

                // Method-level comptime type parameters (e.g. `comptime F: type`)
                // and any later param whose declared type is one of those names
                // (e.g. `f: F`) cannot be resolved here — their concrete types
                // are only known at the method's call site. Match the
                // top-level-generic-fn treatment in declarations.rs: store
                // COMPTIME_TYPE as a placeholder and let specialization fill
                // the concrete type in when the method is monomorphized.
                let type_sym = self.interner.get_or_intern("type");
                let method_type_param_names: Vec<Spur> = params
                    .iter()
                    .filter(|p| p.is_comptime && p.ty == type_sym)
                    .map(|p| p.name)
                    .collect();

                // Build a "sentinel" map that assigns a concrete type to each
                // method-level type param, used to detect whether a given
                // type symbol references any of them (including through
                // array / pointer / tuple wrappers).
                let sentinel_subst: rustc_hash::FxHashMap<Spur, Type> = method_type_param_names
                    .iter()
                    .map(|&n| (n, Type::I32))
                    .collect();
                let references_method_type_param = |sema: &mut Self, ty_sym: Spur| -> bool {
                    if method_type_param_names.contains(&ty_sym) {
                        return true;
                    }
                    let with = sema.resolve_type_for_comptime_with_subst(ty_sym, &sentinel_subst);
                    // Also include the caller's type_subst so `T` from the
                    // outer generic still resolves in the "without" baseline.
                    let without = sema.resolve_type_for_comptime_with_subst(ty_sym, type_subst);
                    with.is_some() && without.is_none()
                };

                for p in params {
                    let type_str = self.interner.resolve(&p.ty);
                    let resolved_ty = if type_str == "Self" {
                        struct_type
                    } else if p.is_comptime && p.ty == type_sym {
                        // `comptime X: type` param — placeholder until call.
                        Type::COMPTIME_TYPE
                    } else if references_method_type_param(self, p.ty) {
                        // Declared type is (or contains) a method-level
                        // comptime type param.
                        Type::COMPTIME_TYPE
                    } else {
                        self.resolve_type_for_comptime_with_subst(p.ty, type_subst)?
                    };
                    param_types.push(resolved_ty);
                }

                let ret_type_str = self.interner.resolve(return_type);
                let ret_type = if ret_type_str == "Self" {
                    struct_type
                } else if references_method_type_param(self, *return_type) {
                    Type::COMPTIME_TYPE
                } else {
                    self.resolve_type_for_comptime_with_subst(*return_type, type_subst)?
                };

                // Preserve mode and comptime flags so specialization can see
                // method-level comptime type parameters.
                let param_range =
                    self.param_arena
                        .alloc(param_names, param_types, param_modes, param_comptime);

                self.methods.insert(
                    key,
                    MethodInfo {
                        struct_type,
                        has_self: *has_self,
                        receiver,
                        params: param_range,
                        return_type: ret_type,
                        body: *body,
                        span: method_inst.span,
                        is_unchecked: *is_unchecked,
                        is_generic: !method_type_param_names.is_empty(),
                        return_type_sym: *return_type,
                    },
                );
            }
        }
        Some(())
    }

    /// Register the single synthesized `__call` method on a lambda-origin
    /// anonymous struct (ADR-0055).
    ///
    /// Unlike `register_anon_struct_methods_for_comptime_with_subst`, we take
    /// Resolve a single comptime type argument supplied at a generic method
    /// call site (ADR-0055). Accepts: a `type` literal (e.g. `i32`), an
    /// anon-struct-type expression evaluated at comptime, a comptime type
    /// variable bound earlier in the function (`let W = Wrap(i32)`), or a
    /// bare struct/enum name.
    pub(crate) fn resolve_method_generic_type_arg(
        &mut self,
        arg: InstRef,
        param_name: Spur,
        ctx: &super::context::AnalysisContext,
    ) -> CompileResult<Type> {
        let arg_inst = self.rir.get(arg);
        match &arg_inst.data {
            InstData::TypeConst { type_name } => self.resolve_type(*type_name, arg_inst.span),
            InstData::AnonStructType { .. } => {
                let empty_type_subst: rustc_hash::FxHashMap<Spur, Type> =
                    rustc_hash::FxHashMap::default();
                let empty_value_subst: rustc_hash::FxHashMap<Spur, ConstValue> =
                    rustc_hash::FxHashMap::default();
                match self.try_evaluate_const_with_subst(arg, &empty_type_subst, &empty_value_subst)
                {
                    Some(ConstValue::Type(ty)) => Ok(ty),
                    _ => Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "method-generic argument for `{}` must be a type value",
                                self.interner.resolve(&param_name)
                            ),
                        },
                        arg_inst.span,
                    )),
                }
            }
            _ => {
                let resolved_ty = if let InstData::VarRef { name } = &arg_inst.data {
                    if let Some(&ty) = ctx.comptime_type_vars.get(name) {
                        Some(ty)
                    } else if let Some(&sid) = self.structs.get(name) {
                        Some(Type::new_struct(sid))
                    } else if let Some(&eid) = self.enums.get(name) {
                        Some(Type::new_enum(eid))
                    } else {
                        None
                    }
                } else {
                    None
                };
                resolved_ty.ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "method-generic argument for `{}` must be a type literal, \
                                 struct/enum name, or comptime type variable",
                                self.interner.resolve(&param_name)
                            ),
                        },
                        arg_inst.span,
                    )
                })
            }
        }
    }

    /// Look up the declared parameter type symbols for a method body. Walks
    /// RIR to find the FnDecl whose `body` matches and returns its params'
    /// declared `ty` Spurs in order. Used by ADR-0055 comptime type-arg
    /// inference, which needs the as-written type names (e.g. `F`, `T`,
    /// `[U; 3]`) rather than the registered types (which are placeholders
    /// for method-level generics).
    pub(crate) fn method_param_type_syms(&self, method_body: InstRef) -> Option<Vec<Spur>> {
        for (_, inst) in self.rir.iter() {
            if let InstData::FnDecl {
                body,
                params_start,
                params_len,
                ..
            } = &inst.data
                && *body == method_body
            {
                let params = self.rir.get_params(*params_start, *params_len);
                return Some(params.iter().map(|p| p.ty).collect());
            }
        }
        None
    }

    /// the method InstRef directly rather than reading it from the RIR extra
    /// array, and there is no comptime-parameter substitution to apply — the
    /// parent function's comptime substitutions have already baked into the
    /// method body during astgen.
    pub(crate) fn register_anon_fn_call_method(
        &mut self,
        struct_id: StructId,
        struct_type: Type,
        method_ref: InstRef,
        span: Span,
    ) -> CompileResult<()> {
        let method_inst = self.rir.get(method_ref);
        let (
            method_name,
            is_unchecked,
            params_start,
            params_len,
            return_type,
            body,
            has_self,
            receiver,
        ) = match &method_inst.data {
            InstData::FnDecl {
                name,
                is_unchecked,
                params_start,
                params_len,
                return_type,
                body,
                has_self,
                receiver_mode,
                ..
            } => (
                *name,
                *is_unchecked,
                *params_start,
                *params_len,
                *return_type,
                *body,
                *has_self,
                crate::sema::anon_interfaces::decode_receiver_mode(*receiver_mode),
            ),
            _ => unreachable!("AnonFnValue method must be a FnDecl"),
        };

        let key = (struct_id, method_name);
        if self.methods.contains_key(&key) {
            return Ok(());
        }

        let params = self.rir.get_params(params_start, params_len);
        let param_names: Vec<Spur> = params.iter().map(|p| p.name).collect();
        let mut param_types: Vec<Type> = Vec::with_capacity(params.len());
        for p in params {
            let type_str = self.interner.resolve(&p.ty);
            let resolved = if type_str == "Self" {
                struct_type
            } else {
                self.resolve_type(p.ty, span)?
            };
            param_types.push(resolved);
        }

        let ret_type_str = self.interner.resolve(&return_type);
        let ret_ty = if ret_type_str == "Self" {
            struct_type
        } else {
            self.resolve_type(return_type, span)?
        };

        let param_range = self.param_arena.alloc_method(param_names, param_types);

        self.methods.insert(
            key,
            MethodInfo {
                struct_type,
                has_self,
                receiver,
                params: param_range,
                return_type: ret_ty,
                body,
                span: method_inst.span,
                is_unchecked,
                // Synthesized __call methods (ADR-0055) don't have their
                // own method-level comptime type params.
                is_generic: false,
                return_type_sym: return_type,
            },
        );
        Ok(())
    }

    /// Register methods for an anonymous enum created via comptime with type substitution.
    ///
    /// Analogous to `register_anon_struct_methods_for_comptime_with_subst`, but for enums.
    /// Resolves method parameter/return types with `Self` mapped to the anonymous enum type.
    fn register_anon_enum_methods_for_comptime_with_subst(
        &mut self,
        enum_id: EnumId,
        enum_type: crate::types::Type,
        methods_start: u32,
        methods_len: u32,
        type_subst: &rustc_hash::FxHashMap<Spur, Type>,
    ) -> Option<()> {
        let method_refs = self.rir.get_inst_refs(methods_start, methods_len);

        let mut seen_methods: rustc_hash::FxHashSet<Spur> = rustc_hash::FxHashSet::default();

        for method_ref in method_refs {
            let method_inst = self.rir.get(method_ref);
            if let InstData::FnDecl {
                name: method_name,
                is_unchecked,
                params_start,
                params_len,
                return_type,
                body,
                has_self,
                receiver_mode,
                ..
            } = &method_inst.data
            {
                let receiver = crate::sema::anon_interfaces::decode_receiver_mode(*receiver_mode);
                let key = (enum_id, *method_name);

                if seen_methods.contains(method_name) {
                    return None;
                }
                seen_methods.insert(*method_name);

                if self.enum_methods.contains_key(&key) {
                    return None;
                }

                let params = self.rir.get_params(*params_start, *params_len);
                let param_names: Vec<Spur> = params.iter().map(|p| p.name).collect();
                let mut param_types: Vec<Type> = Vec::with_capacity(params.len());

                for p in params {
                    let type_str = self.interner.resolve(&p.ty);
                    let resolved_ty = if type_str == "Self" {
                        enum_type
                    } else {
                        self.resolve_type_for_comptime_with_subst(p.ty, type_subst)?
                    };
                    param_types.push(resolved_ty);
                }

                let ret_type_str = self.interner.resolve(return_type);
                let ret_type = if ret_type_str == "Self" {
                    enum_type
                } else {
                    self.resolve_type_for_comptime_with_subst(*return_type, type_subst)?
                };

                let param_range = self
                    .param_arena
                    .alloc_method(param_names.into_iter(), param_types.into_iter());

                self.enum_methods.insert(
                    key,
                    MethodInfo {
                        struct_type: enum_type,
                        has_self: *has_self,
                        receiver,
                        params: param_range,
                        return_type: ret_type,
                        body: *body,
                        span: method_inst.span,
                        is_unchecked: *is_unchecked,
                        // Method-level comptime type params on enum methods
                        // are not yet supported; set to false.
                        is_generic: false,
                        return_type_sym: *return_type,
                    },
                );
            }
        }
        Some(())
    }

    /// Extract method signatures from RIR for structural equality comparison.
    ///
    /// This extracts method signatures as type symbols (Spur), not resolved Types.
    /// This is intentional: for structural equality, we compare type symbols directly
    /// so that `Self` matches `Self` even before we know the concrete StructId.
    fn extract_anon_method_sigs(
        &self,
        methods_start: u32,
        methods_len: u32,
    ) -> Vec<super::AnonMethodSig> {
        let method_refs = self.rir.get_inst_refs(methods_start, methods_len);
        let mut sigs = Vec::with_capacity(method_refs.len());

        for method_ref in method_refs {
            let method_inst = self.rir.get(method_ref);
            if let InstData::FnDecl {
                name,
                params_start,
                params_len,
                return_type,
                has_self,
                ..
            } = &method_inst.data
            {
                // Extract parameter types as symbols (excluding self)
                let params = self.rir.get_params(*params_start, *params_len);
                let param_types: Vec<Spur> = params.iter().map(|p| p.ty).collect();

                sigs.push(super::AnonMethodSig {
                    name: *name,
                    has_self: *has_self,
                    param_types,
                    return_type: *return_type,
                });
            }
        }

        sigs
    }

    // ========================================================================
    // Pointer intrinsics (require unchecked context)
    // ========================================================================

    /// Analyze @ptr_read intrinsic: reads value through pointer.
    /// Signature: @ptr_read(ptr: ptr const T) -> T
    fn analyze_ptr_read_intrinsic(
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
    fn analyze_ptr_write_intrinsic(
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
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: self.format_type_name(pointee_type),
                    found: self.format_type_name(value_type),
                },
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
    fn analyze_ptr_offset_intrinsic(
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
    fn analyze_ptr_to_int_intrinsic(
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
    fn analyze_int_to_ptr_intrinsic(
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
    fn analyze_null_ptr_intrinsic(
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
    fn analyze_is_null_intrinsic(
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
    fn analyze_ptr_copy_intrinsic(
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
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: self.format_type_name(dst_pointee),
                    found: self.format_type_name(src_pointee),
                },
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
    fn analyze_addr_of_intrinsic(
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
    pub(crate) fn lower_pointer_op_to_air(
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
            // p.read() — receiver: pointer; args: 0; result: pointee T
            IntrinsicId::PtrRead => {
                if !args.is_empty() {
                    return Err(self.pointer_op_arg_count_err(op_name, 0, args.len(), span));
                }
                let recv = receiver.expect("read is a method");
                let pointee = self.pointer_pointee_type(recv.ty);
                let air_ref = make_intrinsic(air, &[recv.air_ref.as_u32()], pointee);
                Ok(AnalysisResult::new(air_ref, pointee))
            }
            // p.write(v) — receiver: MutPtr(T); args: [v: T]; result: ()
            IntrinsicId::PtrWrite => {
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
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: self.format_type_name(pointee),
                            found: self.format_type_name(v.ty),
                        },
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
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: self.format_type_name(dst_pointee),
                            found: self.format_type_name(src_pointee),
                        },
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
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: self.format_type_name(pointee_from_lhs),
                            found: self.format_type_name(ref_pointee),
                        },
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

fn pointer_kind_name(kind: PointerKind) -> &'static str {
    match kind {
        PointerKind::Ptr => "Ptr",
        PointerKind::MutPtr => "MutPtr",
    }
}

impl<'a> Sema<'a> {
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

        // Determine variant index based on host architecture (compile-time evaluation)
        // Currently we always compile for the host architecture
        let variant_index = match gruel_target::Target::host().arch() {
            Arch::X86_64 => 0,
            Arch::Aarch64 => 1,
        };

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

        // Determine variant index based on host OS (compile-time evaluation)
        // Currently we always compile for the host OS
        let variant_index = match gruel_target::Target::host().os() {
            Os::Linux => 0,
            Os::Macos => 1,
        };

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

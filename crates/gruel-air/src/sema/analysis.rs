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

use gruel_rir::{InstData, InstRef, RirArgMode, RirCallArg, RirDirective, RirParamMode};
use gruel_target::{Arch, Os};

/// Maps a target [`Arch`] to its variant index in `ARCH_ENUM` from
/// `gruel-builtins`. The order is the historical compatibility order:
/// existing programs depend on `X86_64 = 0`, `Aarch64 = 1`, with new
/// variants appended.
pub(super) fn arch_variant_index(arch: Arch) -> u32 {
    match arch {
        Arch::X86_64 => 0,
        Arch::Aarch64 => 1,
        Arch::X86 => 2,
        Arch::Arm => 3,
        Arch::Riscv32 => 4,
        Arch::Riscv64 => 5,
        Arch::Wasm32 => 6,
        Arch::Wasm64 => 7,
        // Unknown architectures fall back to X86_64 so a stray triple
        // doesn't ICE; users targeting an unrecognized arch should
        // notice via the unblessed-target warning.
        Arch::Unknown => 0,
    }
}

/// Maps a target [`Os`] to its variant index in `OS_ENUM` from
/// `gruel-builtins`. The order is the historical compatibility order:
/// existing programs depend on `Linux = 0`, `Macos = 1`, with new
/// variants appended.
pub(super) fn os_variant_index(os: Os) -> u32 {
    match os {
        Os::Linux => 0,
        Os::Macos => 1,
        Os::Windows => 2,
        Os::Freestanding => 3,
        Os::Wasi => 4,
        Os::Unknown => 0,
    }
}
use gruel_util::{BinOp, Span};
use gruel_util::{
    CompileError, CompileErrors, CompileResult, CompileWarning, ErrorKind, MultiErrorResult,
    OptionExt, PreviewFeature, WarningKind,
};
use lasso::Spur;

use super::context::{AnalysisContext, AnalysisResult, ComptimeHeapItem, ConstValue, ParamInfo};
use super::{AnalyzedFunction, InferenceContext, MethodInfo, Sema, SemaOutput};
use crate::inference::{
    Constraint, ConstraintContext, ConstraintGenerator, ParamVarInfo, Unifier, UnifyResult,
};
use crate::inst::{
    Air, AirArgMode, AirCallArg, AirInst, AirInstData, AirPlaceBase, AirProjection, AirRef,
};
use crate::types::{EnumId, EnumVariantDef, StructField, StructId, Type, TypeKind};

/// Data describing a method body for analysis.
struct MethodBodySpec<'a> {
    return_type: Spur,
    params: &'a [gruel_rir::RirParam],
    body: InstRef,
    /// The host struct/enum type. Always set when analyzing a method or
    /// associated function inside a `struct` / `enum` body, regardless of
    /// whether the function has a `self` parameter. ADR-0076 uses this to
    /// bind `Self` for the duration of method-body analysis so that
    /// associated functions like `fn new() -> Self` resolve `Self` to the
    /// host type. `None` when analyzing a free top-level function.
    host_type: Option<Type>,
    /// Whether the function has a `self` parameter (i.e., it is a method
    /// rather than an associated function). When `true`, `self` is added
    /// as the first parameter with `host_type` as its type.
    has_self: bool,
    /// Receiver shape (ADR-0076 sole form: `self` / `self : MutRef(Self)` /
    /// `self : Ref(Self)`). Encoded as a byte: 0 = by-value, 1 =
    /// `MutRef(Self)`, 2 = `Ref(Self)`. Ignored when `has_self` is `false`.
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
pub(super) struct AnonStructSpec {
    pub(super) struct_id: StructId,
    pub(super) struct_type: crate::types::Type,
    pub(super) methods_start: u32,
    pub(super) methods_len: u32,
}

/// Main entry point for analyzing all function bodies (ADR-0026).
///
/// Implements "lazy semantic analysis": only functions reachable from the
/// entry point (`main`) are analyzed. Unreferenced code is not analyzed,
/// not codegen'd, and errors in unreferenced code are not reported.
///
/// This is the same trade-off Zig makes for faster builds and smaller binaries.
pub(crate) fn analyze_all_function_bodies(mut sema: Sema<'_>) -> MultiErrorResult<SemaOutput> {
    let sema = &mut sema;
    // Build inference context once
    let infer_ctx = sema.build_inference_context();

    // Work queue: functions/methods to analyze. Seed it with main() — the
    // entry point — and let reachability pull in the rest. The compiler
    // driver enforces that main() exists at codegen time
    // (`ErrorKind::NoMainFunction`), so sema doesn't need to. When no main
    // exists (e.g. golden-CFG spec tests over a single fn), fall back to
    // enqueueing every non-generic top-level function so analysis still
    // covers the program.
    let mut pending_functions: Vec<Spur> = match sema.interner.get("main") {
        Some(sym) if sema.functions.contains_key(&sym) => vec![sym],
        _ => sema
            .functions
            .iter()
            .filter_map(|(name, info)| (!info.is_generic).then_some(*name))
            .collect(),
    };
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

    // Drive analysis to a fixed point: lazy BFS of reachable
    // functions/methods, one-shot post-processing (vtables, anon types,
    // destructors, derives, inline drops), then specialization. Specialization
    // synthesizes new bodies whose method/function references we feed back
    // into the work queue, so we re-enter the loop until nothing new appears.
    //
    // The specialization name map persists across outer iterations so that a
    // newly-analyzed body with a CallGeneric for an already-synthesized key
    // (e.g. a method whose body re-calls a generic main has already
    // specialized) doesn't trigger a duplicate body — `specialize` skips keys
    // it has seen before but still rewrites the new CallGeneric to a Call.
    let mut post_processed = false;
    let mut spec_name_map: rustc_hash::FxHashMap<crate::specialize::SpecializationKey, Spur> =
        rustc_hash::FxHashMap::default();
    loop {
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

                // Look up the method info — first in struct methods, then in
                // enum methods. Enum methods are recorded with the same
                // `(StructId, Spur)` key shape (StructId reusing the EnumId's
                // raw u32) so the work queue can dispatch them uniformly.
                let (method_info, is_enum_method) =
                    if let Some(info) = sema.methods.get(&(struct_id, method_name)) {
                        (*info, false)
                    } else if let Some(info) = sema
                        .enum_methods
                        .get(&(crate::types::EnumId(struct_id.0), method_name))
                    {
                        (*info, true)
                    } else {
                        continue;
                    };

                // Method-level generic: defer body analysis to specialization.
                if method_info.is_generic {
                    continue;
                }

                // Get the type definition to find its name for impl block lookup
                let type_name_str = if is_enum_method {
                    sema.type_pool
                        .enum_def(crate::types::EnumId(struct_id.0))
                        .name
                        .clone()
                } else {
                    sema.type_pool.struct_def(struct_id).name.clone()
                };
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
                        let self_mode = match method_info.receiver {
                            crate::types::ReceiverMode::ByValue => RirParamMode::Normal,
                            crate::types::ReceiverMode::Ref => RirParamMode::Ref,
                            crate::types::ReceiverMode::MutRef => RirParamMode::MutRef,
                        };
                        param_info.push((self_sym, method_info.struct_type, self_mode));
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

                // ADR-0078: enum methods (named enums declared in source — not
                // anonymous, those are handled by the fixed-point loop below).
                // Find the matching `fn` inside the enum's declaration and
                // analyze its body, mirroring the struct branch below.
                if is_enum_method {
                    for (_, inst) in sema.rir.iter() {
                        if let InstData::EnumDecl {
                            name: enum_name,
                            methods_start,
                            methods_len,
                            ..
                        } = &inst.data
                            && *enum_name == type_name_sym
                        {
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
                                    && *m_name == method_name
                                {
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
                                            host_type: Some(method_info.struct_type),
                                            has_self: *has_self,
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
                                        host_type: Some(method_info.struct_type),
                                        has_self: *has_self,
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

        // Post-processing is one-shot — derives, destructors, vtables, and the
        // anonymous-method fixed points each enumerate sema state directly and
        // would re-emit duplicate analyzed bodies on a second pass. A labeled
        // block lets later outer-loop iterations skip the whole section after
        // the first round without re-indenting the existing code.
        'post_processing: {
            if post_processed {
                break 'post_processing;
            }

            // ADR-0078: catch-all for anonymous struct methods registered after the
            // work queue drained. Specialization (for `comptime F: type` parameters
            // bound to anonymous-struct callable types) and other late-registration
            // paths can leave `__call` methods unanalyzed if their references
            // weren't tracked through `ctx.referenced_methods`. The fixed-point
            // loop here mirrors the anonymous-enum fixed-point loop just below.
            let mut analyzed_anon_struct_methods: HashSet<(StructId, Spur)> = HashSet::default();
            // Pre-seed with the methods the work queue already analyzed so we don't
            // double-emit.
            for (struct_id, method_name) in &analyzed_methods {
                analyzed_anon_struct_methods.insert((*struct_id, *method_name));
            }
            loop {
                let pending_anon_struct_methods: Vec<(StructId, Spur, MethodInfo)> = sema
                    .methods
                    .iter()
                    .filter_map(|((struct_id, method_name), method_info)| {
                        let struct_def = sema.type_pool.struct_def(*struct_id);
                        if struct_def.name.starts_with("__anon_struct_")
                            && !analyzed_anon_struct_methods.contains(&(*struct_id, *method_name))
                            && !method_info.is_generic
                        {
                            Some((*struct_id, *method_name, *method_info))
                        } else {
                            None
                        }
                    })
                    .collect();
                if pending_anon_struct_methods.is_empty() {
                    break;
                }
                for (struct_id, method_name, method_info) in pending_anon_struct_methods {
                    analyzed_anon_struct_methods.insert((struct_id, method_name));
                    let struct_def = sema.type_pool.struct_def(struct_id);
                    let type_name_str = struct_def.name.clone();
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
                        let self_mode = match method_info.receiver {
                            crate::types::ReceiverMode::ByValue => RirParamMode::Normal,
                            crate::types::ReceiverMode::Ref => RirParamMode::Ref,
                            crate::types::ReceiverMode::MutRef => RirParamMode::MutRef,
                        };
                        param_info.push((self_sym, method_info.struct_type, self_mode));
                    }
                    for i in 0..param_names.len() {
                        param_info.push((param_names[i], param_types[i], param_modes[i]));
                    }
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
                        let self_mode = match method_info.receiver {
                            crate::types::ReceiverMode::ByValue => RirParamMode::Normal,
                            crate::types::ReceiverMode::Ref => RirParamMode::Ref,
                            crate::types::ReceiverMode::MutRef => RirParamMode::MutRef,
                        };
                        param_info.push((self_sym, method_info.struct_type, self_mode));
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

            // ADR-0056 vtable population: every (struct, interface) pair recorded
            // in `interface_vtables_needed` references the conformer's slot
            // methods. Codegen emits the vtable, so those methods must be in the
            // analyzed functions list. Queue them for analysis if the work loop
            // didn't already pick them up.
            let vtable_methods: Vec<(StructId, Spur)> = sema
                .interface_vtables_needed
                .values()
                .flat_map(|witness| witness.iter().copied())
                .collect();
            for (struct_id, method_name) in vtable_methods {
                if !analyzed_methods.contains(&(struct_id, method_name)) {
                    pending_methods.push((struct_id, method_name));
                }
            }
            // Drain again now that vtable methods may have been queued.
            while !pending_methods.is_empty() {
                let queue = std::mem::take(&mut pending_methods);
                let mut local_pending = queue;
                while let Some((struct_id, method_name)) = local_pending.pop() {
                    if analyzed_methods.contains(&(struct_id, method_name)) {
                        continue;
                    }
                    analyzed_methods.insert((struct_id, method_name));

                    let (method_info, is_enum_method) =
                        if let Some(info) = sema.methods.get(&(struct_id, method_name)) {
                            (*info, false)
                        } else if let Some(info) = sema
                            .enum_methods
                            .get(&(crate::types::EnumId(struct_id.0), method_name))
                        {
                            (*info, true)
                        } else {
                            continue;
                        };
                    if method_info.is_generic {
                        continue;
                    }

                    let type_name_str = if is_enum_method {
                        sema.type_pool
                            .enum_def(crate::types::EnumId(struct_id.0))
                            .name
                            .clone()
                    } else {
                        sema.type_pool.struct_def(struct_id).name.clone()
                    };
                    let type_name_sym = sema.interner.get_or_intern(&type_name_str);
                    let method_name_str = sema.interner.resolve(&method_name).to_string();

                    // Find the FnDecl in either struct or enum declarations and
                    // analyze its body. (Anon types are handled by the fixed-point
                    // loops below.)
                    let decl_iter: Vec<_> = sema.rir.iter().collect();
                    let mut handled = false;
                    for (_, inst) in decl_iter {
                        let (decl_name, methods_start, methods_len) = match &inst.data {
                            InstData::StructDecl {
                                name,
                                methods_start,
                                methods_len,
                                ..
                            } if !is_enum_method => (*name, *methods_start, *methods_len),
                            InstData::EnumDecl {
                                name,
                                methods_start,
                                methods_len,
                                ..
                            } if is_enum_method => (*name, *methods_start, *methods_len),
                            _ => continue,
                        };
                        if decl_name != type_name_sym {
                            continue;
                        }
                        let methods = sema.rir.get_inst_refs(methods_start, methods_len);
                        for method_ref in methods {
                            let method_inst = sema.rir.get(method_ref);
                            let InstData::FnDecl {
                                name: m_name,
                                params_start,
                                params_len,
                                return_type,
                                body,
                                has_self,
                                receiver_mode,
                                ..
                            } = &method_inst.data
                            else {
                                continue;
                            };
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
                                    host_type: Some(method_info.struct_type),
                                    has_self: *has_self,
                                    self_mode: *receiver_mode,
                                },
                                method_inst.span,
                            ) {
                                Ok((analyzed, warnings, local_strings, local_bytes, _, _)) => {
                                    functions_with_strings.push((
                                        analyzed,
                                        local_strings,
                                        local_bytes,
                                    ));
                                    all_warnings.extend(warnings);
                                }
                                Err(e) => errors.push(e),
                            }
                            handled = true;
                            break;
                        }
                        if handled {
                            break;
                        }
                    }
                }
            }

            // ADR-0058: derive-bound methods spliced onto host types via
            // `@derive(...)` directives. Same as the sequential path; the work
            // queue doesn't reach these because they aren't discovered through
            // direct call lookup.
            let derive_jobs: Vec<(Spur, Spur, bool, super::DeriveBinding)> = sema
                .derive_bindings
                .iter()
                .map(|b| (b.derive_name, b.host_name, b.host_is_enum, *b))
                .collect();
            for (derive_name, host_name, host_is_enum, _binding) in derive_jobs {
                let dmethods: Vec<crate::sema::info::DeriveMethod> =
                    match sema.derives.get(&derive_name) {
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
                                host_type: Some(enum_type),
                                has_self: *has_self,
                                self_mode: *receiver_mode,
                            },
                            m.span,
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
                                // ADR-0081: feed references from the derived
                                // body back into the work queue so symbols
                                // it depends on (e.g. `String.clone` from a
                                // `@derive(Clone)` body's `@field(...).clone()`
                                // calls) get analyzed and emitted.
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
                                host_type: Some(struct_type),
                                has_self: *has_self,
                                self_mode: *receiver_mode,
                            },
                            m.span,
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
                                // ADR-0081: feed references from the derived
                                // body back into the work queue.
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

            // ADR-0053 phase 3 / 3b: also analyze inline `fn drop(self)` destructors
            // (struct- or enum-body declared) — same as the sequential path. The
            // lazy work queue doesn't reach these because the methods aren't
            // discovered through call dispatch.
            let inline_struct_drops: Vec<(StructId, InstRef, Span)> = sema
                .inline_struct_drops
                .iter()
                .map(|(sid, (body, span))| (*sid, *body, *span))
                .collect();
            for (struct_id, body, drop_span) in inline_struct_drops {
                let struct_def = sema.type_pool.struct_def(struct_id);
                let type_name_str = struct_def.name.clone();
                let full_name = format!("{}.__drop", type_name_str);
                let struct_type = Type::new_struct(struct_id);
                match sema.analyze_destructor_function(
                    &infer_ctx,
                    &full_name,
                    body,
                    drop_span,
                    struct_type,
                ) {
                    Ok((analyzed, warnings, local_strings, local_bytes, _ref_fns, _ref_meths)) => {
                        functions_with_strings.push((analyzed, local_strings, local_bytes));
                        all_warnings.extend(warnings);
                    }
                    Err(e) => errors.push(e),
                }
            }
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
                match sema
                    .analyze_destructor_function(&infer_ctx, &full_name, body, drop_span, enum_type)
                {
                    Ok((analyzed, warnings, local_strings, local_bytes, _ref_fns, _ref_meths)) => {
                        functions_with_strings.push((analyzed, local_strings, local_bytes));
                        all_warnings.extend(warnings);
                    }
                    Err(e) => errors.push(e),
                }
            }

            post_processed = true;
        }

        // Defensive: post-processing's inner loops drain their own work, but
        // re-enter the outer loop if anything is still pending so the BFS can
        // absorb it before specialization runs.
        if !pending_functions.is_empty() || !pending_methods.is_empty() {
            continue;
        }

        // Run specialization. It collects every CallGeneric across the
        // analyzed bodies, rewrites them to direct Call instructions, and
        // synthesizes specialized bodies — re-running internally until no
        // new specialization keys appear. The references it returns are
        // method/function names the synthesized bodies depend on; we feed
        // them back into the work queue so reachability stays closed.
        let refs = match crate::specialize::specialize(
            &mut functions_with_strings,
            &mut spec_name_map,
            sema,
            &infer_ctx,
            sema.interner,
        ) {
            Ok(refs) => refs,
            Err(e) => {
                errors.push(e);
                crate::specialize::SpecializationRefs::default()
            }
        };

        let mut had_new = false;
        for f in refs.fns {
            if !analyzed_functions.contains(&f) {
                pending_functions.push(f);
                had_new = true;
            }
        }
        for m in refs.meths {
            if !analyzed_methods.contains(&m) {
                pending_methods.push(m);
                had_new = true;
            }
        }

        if !had_new {
            break;
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

    let output = SemaOutput {
        functions,
        strings: global_strings,
        bytes: global_bytes,
        warnings: all_warnings,
        type_pool: sema.type_pool.clone(),
        comptime_dbg_output: std::mem::take(&mut sema.comptime_dbg_output),
        interface_defs: sema.interface_defs.clone(),
        interface_vtables: sema.interface_vtables_needed.clone(),
    };

    // Surface anonymous-host derive expansion errors (ADR-0058).
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

    /// ADR-0073: cross-module field visibility check.
    ///
    /// Non-`pub` fields are accessible only from inside the type's home
    /// module. Built-ins are homed in the synthetic `<builtin>` file, so
    /// their non-`pub` fields are unreachable from user code.
    pub(crate) fn check_field_visibility(
        &self,
        struct_def: &crate::types::StructDef,
        field: &crate::types::StructField,
        access_span: Span,
    ) -> CompileResult<()> {
        let accessing_file_id = access_span.file_id;
        let target_file_id = struct_def.file_id;
        if !self.is_accessible(accessing_file_id, target_file_id, field.is_pub) {
            return Err(CompileError::new(
                ErrorKind::PrivateField {
                    struct_name: struct_def.name.clone(),
                    field_name: field.name.clone(),
                },
                access_span,
            ));
        }
        Ok(())
    }

    /// ADR-0073: cross-module method visibility check.
    ///
    /// Non-`pub` methods are callable only from inside the type's home
    /// module.
    pub(crate) fn check_method_visibility(
        &self,
        type_name: &str,
        _is_builtin: bool,
        method_is_pub: bool,
        method_file_id: gruel_util::FileId,
        method_name: &str,
        access_span: Span,
    ) -> CompileResult<()> {
        let accessing_file_id = access_span.file_id;
        if !self.is_accessible(accessing_file_id, method_file_id, method_is_pub) {
            return Err(CompileError::new(
                ErrorKind::PrivateMemberAccess {
                    item_kind: "method".to_string(),
                    name: format!("{}::{}", type_name, method_name),
                },
                access_span,
            ));
        }
        Ok(())
    }

    /// Check that we are inside a `checked` block.
    /// Returns an error if `checked_depth` is zero.
    pub(crate) fn require_checked_for_intrinsic(
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
        // interface-typed parameters (ADR-0056 Phase 4) and `Ref(I)` /
        // `MutRef(I)` interface refs (ADR-0076 Phase 2) resolve correctly.
        // The returned mode may differ from `p.mode` after normalization.
        let param_info: Vec<(Spur, Type, RirParamMode)> = params
            .iter()
            .map(|p| {
                let (ty, mode) = self.resolve_param_type(p.ty, p.mode, span)?;
                Ok((p.name, ty, mode))
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
        // ADR-0076: bind `Self` for the duration of body analysis whenever
        // we are inside a struct/enum body — including associated functions
        // like `fn new() -> Self` that have no `self` parameter. The host
        // type is what `Self` refers to in `Self::Variant`, `Self { ... }`,
        // and any wrapped form (`Vec(Self)`, `Ref(Self)`, …).
        let saved_self = self.current_self;
        if let Some(host) = spec.host_type {
            self.current_self = Some(host);
        }

        let ret_type = self.resolve_type(spec.return_type, span)?;

        // Build parameter list, adding self as first parameter for methods
        let mut param_info: Vec<(Spur, Type, RirParamMode)> = Vec::new();

        if spec.has_self {
            let host = spec
                .host_type
                .expect("MethodBodySpec.has_self=true requires host_type to be set");
            // ADR-0076: encode the receiver shape directly in the synthesized
            // self parameter's type — the byte-encoded receiver mode set by
            // the parser (1 = `MutRef(Self)`, 2 = `Ref(Self)`, 0 = by-value)
            // becomes a `MutRef(Self)` / `Ref(Self)` / `Self` type with a
            // `Normal` parameter mode. Body analysis, borrow tracking, and
            // codegen all key off the type pool from this point forward.
            let self_ty = match spec.self_mode {
                1 => Type::new_mut_ref(self.type_pool.intern_mut_ref_from_type(host)),
                2 => Type::new_ref(self.type_pool.intern_ref_from_type(host)),
                _ => host,
            };
            let self_sym = self.interner.get_or_intern("self");
            param_info.push((self_sym, self_ty, RirParamMode::Normal));
        }

        // Add regular parameters with their modes. Use `resolve_param_type`
        // for ADR-0056 interface-typed parameters; ADR-0076 Phase 2 also
        // normalizes `Ref(I)` / `MutRef(I)` here, so the returned mode may
        // differ from `p.mode`.
        for p in spec.params.iter() {
            let (ty, mode) = self.resolve_param_type(p.ty, p.mode, span)?;
            param_info.push((p.name, ty, mode));
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

        self.current_self = saved_self;

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
        // ADR-0076: bind `Self` to the host struct/enum while analyzing the
        // destructor body so `Self::Variant` / `Self { ... }` resolve.
        let saved_self = self.current_self.replace(struct_type);

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

        self.current_self = saved_self;

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
        // ADR-0076 internal collapse: bindings keep their surface
        // `Ref(T)` / `MutRef(T)` types end-to-end. Body-analysis sites
        // (HM, sema, CFG/codegen) read ref-ness off the type pool
        // (`TypeKind::Ref` / `TypeKind::MutRef`) instead of off a
        // parallel mode field. Auto-deref happens at the use site.

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
        let mut comptime_type_vars = type_subst.cloned().unwrap_or_default();
        // ADR-0076: pervasive `Self`. When analyzing a method/associated-fn
        // body with a host type in scope, expose `Self` to the body's name
        // resolution machinery so struct literals (`Self { ... }`), enum
        // variant paths (`Self::Variant`), and pattern paths
        // (`Self::Variant(x)`) all resolve to the host type.
        if let Some(host) = self.current_self {
            let self_sym = self.interner.get_or_intern("Self");
            comptime_type_vars.entry(self_sym).or_insert(host);
        }
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
            borrow_arg_skip_move: None,
            uninit_handles: HashMap::default(),
            unroll_arm_bindings: HashMap::default(),
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
            .map(|(name, ty, mode)| {
                (
                    *name,
                    ParamVarInfo {
                        ty: self.type_to_infer_type(*ty),
                        mode: *mode,
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
                        mode: RirParamMode::Comptime,
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
        // ADR-0076: auto-deref `Ref(T)` / `MutRef(T)` body values so
        // implicit returns from a `Ref(T)`-typed binding constrain
        // against `T` (sema separately rejects moving out of a borrow).
        let body_ty = match &body_info.ty {
            crate::inference::InferType::Concrete(t) => match t.kind() {
                crate::types::TypeKind::Ref(id) => {
                    crate::inference::InferType::Concrete(self.type_pool.ref_def(id))
                }
                crate::types::TypeKind::MutRef(id) => {
                    crate::inference::InferType::Concrete(self.type_pool.mut_ref_def(id))
                }
                _ => body_info.ty.clone(),
            },
            _ => body_info.ty.clone(),
        };
        cgen.add_constraint(Constraint::equal(
            body_ty,
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
                    return Err(CompileError::use_after_move(
                        name_str, inst.span, moved_span,
                    ));
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
                    return Err(CompileError::use_after_move(
                        name_str, inst.span, moved_span,
                    ));
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
            // ADR-0076: auto-deref through `Ref(T)` / `MutRef(T)` so that
            // `r.field` works in projection contexts (e.g., comparison
            // operands) the same way it does in expression position.
            let base_type = crate::sema::analyze_ops::unwrap_ref_for_place(self, base_result.ty);

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

            // ADR-0073: unified visibility check.
            self.check_field_visibility(&struct_def, struct_field, inst.span)?;

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
            // ADR-0076: auto-deref through `Ref(T)` / `MutRef(T)` so that
            // `arr[i]` works in projection contexts (e.g., comparison
            // operands) when `arr` is a reference parameter. The base's
            // air_ref still points at the param load (the pointer), and
            // codegen treats by-ref params as the base pointer for GEP.
            let base_type = crate::sema::analyze_ops::unwrap_ref_for_place(self, base_result.ty);

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
                return Err(CompileError::type_mismatch(
                    "usize".to_string(),
                    index_result.ty.name().to_string(),
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
            | InstData::CharConst(_)
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
                // ADR-0079 Phase 3: don't clear the heap — a nested
                // `comptime_unroll for` (e.g. one that iterates
                // `v.fields` inside an outer arm-template iteration
                // over `variants`) would otherwise invalidate the
                // outer loop's comptime binding (a `Struct(heap_idx)`
                // pointing at the now-cleared heap). Use the
                // heap-preserving evaluator instead.
                let iterable_val = {
                    let prev_steps = self.comptime_steps_used;
                    self.comptime_steps_used = 0;
                    let mut locals = ctx.comptime_value_vars.clone();
                    let v = self.evaluate_comptime_inst(iterable, &mut locals, ctx, span)?;
                    self.comptime_steps_used = prev_steps;
                    v
                };

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

                        is_pub: true,
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
                return Err(CompileError::use_after_move(root_name, span, moved_span));
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

            // ADR-0073: unified visibility check on field write.
            self.check_field_visibility(&struct_def, struct_field, span)?;

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
                return Err(CompileError::use_after_move(root_name, span, moved_span));
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
                return Err(CompileError::type_mismatch(
                    "usize".to_string(),
                    index_result.ty.name().to_string(),
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

        // Analyze the receiver expression
        let receiver_result = self.analyze_inst(air, receiver, ctx)?;
        let receiver_type = receiver_result.ty;

        // Handle module member access: module.function() becomes a direct function call
        if receiver_type.is_module() {
            return self.analyze_module_member_call_impl(air, method, args, span, ctx);
        }

        // ADR-0079: `.clone()` on a Copy type is a bitwise copy. The
        // method dispatch falls through to "no method named clone"
        // for primitives like i32, but Copy types structurally
        // conform to `Clone` (lang-item-driven short-circuit), so a
        // `.clone()` call on them must succeed and just hand back the
        // receiver value. Used by the prelude `derive Clone` body to
        // clone Copy fields without requiring per-primitive method
        // declarations.
        if method_name_str == "clone"
            && args.is_empty()
            && self.is_type_copy(receiver_type)
            && self.lang_items.clone().is_some()
        {
            return Ok(AnalysisResult::new(receiver_result.air_ref, receiver_type));
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
            // ADR-0026 lazy analysis: track this enum method as referenced
            // so its body is analyzed by the work queue. The same fix as
            // the struct-method path above.
            ctx.referenced_methods.insert((StructId(enum_id.0), method));

            if !method_info.has_self {
                return Err(CompileError::new(
                    ErrorKind::AssocFnCalledAsMethod {
                        type_name: enum_name_str,
                        function_name: method_name_str,
                    },
                    span,
                ));
            }

            // ADR-0073: gated cross-module visibility check on the enum
            // method. Enums aren't built-in, so the synthetic-builtin
            // exemption doesn't apply.
            self.check_method_visibility(
                &enum_def.name,
                false,
                method_info.is_pub,
                method_info.file_id,
                &method_name_str,
                span,
            )?;

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
                crate::types::ReceiverMode::Ref => AirArgMode::Ref,
                crate::types::ReceiverMode::MutRef => AirArgMode::MutRef,
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
                    return Err(CompileError::type_mismatch(
                        expected_ty.name().to_string(),
                        actual_ty.name().to_string(),
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

        // ADR-0071: methods on `char` values. `to_u32` lowers to a no-op
        // bitcast (the i32 storage already holds the codepoint). `len_utf8`,
        // `is_ascii`, `encode_utf8` are added in later phases.
        if receiver_type == Type::CHAR {
            return self.dispatch_char_method_call(
                air,
                receiver_result,
                &method_name_str,
                &args,
                span,
                ctx,
            );
        }

        // ADR-0066: methods on `Vec(T)` values dispatch through the
        // vec_methods module which emits the appropriate intrinsic. Most
        // Vec methods take borrow/inout self — undo the move that
        // analyze_inst recorded. ADR-0067's `dispose(self)` consumes
        // self by-value, so the move recorded by `analyze_inst` is correct
        // and should be preserved.
        if matches!(receiver_type.kind(), TypeKind::Vec(_)) {
            let consumes_self = matches!(method_name_str.as_str(), "dispose");
            if !consumes_self && let Some(var) = receiver_var {
                ctx.moved_vars.remove(&var);
            }
            return self.dispatch_vec_method_call(
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

        // Look up the struct name by its ID (for error messages)
        let struct_def = self.type_pool.struct_def(struct_id);
        let struct_name_str = struct_def.name.clone();

        // ADR-0065 Phase 2: `@derive(Clone)` structs have a synthesized
        // `<TypeName>.clone(borrow self) -> Self` emitted by `clone_glue`.
        // The synthesized function isn't registered in `self.methods`; emit
        // the Call directly when dispatching `.clone()` on such a struct,
        // and *only* if the user hasn't also written their own clone method
        // (which takes precedence via the regular methods.get path below).
        if struct_def.is_clone
            && method_name_str == "clone"
            && !self.methods.contains_key(&(struct_id, method))
            && args.is_empty()
        {
            // Receiver is `borrow self`; the AIR Call carries the receiver
            // as a Borrow-mode arg.
            if let Some(var) = receiver_var {
                ctx.moved_vars.remove(&var);
            }
            let extra = [receiver_result.air_ref.as_u32(), AirArgMode::Ref.as_u32()];
            let args_start = air.add_extra(&extra);
            let fn_name = self
                .interner
                .get_or_intern(format!("{}.clone", struct_name_str));
            let return_type = Type::new_struct(struct_id);
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Call {
                    name: fn_name,
                    args_start,
                    args_len: 1,
                },
                ty: return_type,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, return_type));
        }

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
        // ADR-0026 lazy analysis (and ADR-0078 prelude module loading):
        // record the dispatched method as referenced so the lazy work
        // queue analyzes its body. Without this, anonymous-struct
        // `__call` methods (and any other dispatched-by-method-call
        // method) are registered but their bodies never get codegen,
        // causing link errors.
        ctx.referenced_methods.insert(method_key);
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

        // ADR-0073: gated cross-module visibility check on the method.
        let struct_def = self.type_pool.struct_def(struct_id);
        self.check_method_visibility(
            &struct_def.name,
            struct_def.is_builtin,
            method_info.is_pub,
            method_info.file_id,
            &method_name_str,
            span,
        )?;

        // ADR-0072 + ADR-0081: the prelude `String` has a `checked`-only
        // bridge surface (`push_byte`, `terminated_ptr`,
        // `from_utf8_unchecked`, `from_c_str_unchecked`). The registry-
        // driven method dispatch is gone, so apply the same name-based
        // gates here.
        self.check_string_vec_bridge_method_gates(&struct_def.name, &method_name_str, ctx, span)?;

        // ADR-0062: a `&self` / `&mut self` receiver is sugar for a borrow
        // (immutable / mutable). The receiver expression's `analyze_inst`
        // already recorded a move on the root variable since it was
        // evaluated as a value; undo it so the caller can keep using the
        // value after the call. This mirrors the interface-dispatch and
        // builtin-method paths above.
        let recv_pass_mode = match method_info.receiver {
            crate::types::ReceiverMode::ByValue => AirArgMode::Normal,
            crate::types::ReceiverMode::Ref => AirArgMode::Ref,
            crate::types::ReceiverMode::MutRef => AirArgMode::MutRef,
        };
        if !matches!(method_info.receiver, crate::types::ReceiverMode::ByValue)
            && let Some(var) = receiver_var
        {
            ctx.moved_vars.remove(&var);
        }

        // ADR-0081: a `MutRef(Self)` receiver requires the bound variable
        // to be `let mut` (or to come through a `MutRef(_)` parameter /
        // local). Without this, `let s = String::new(); s.push_str("...")`
        // would silently mutate `s` despite the `let`'s implicit-immutable
        // binding.
        if matches!(method_info.receiver, crate::types::ReceiverMode::MutRef)
            && let Some(var) = receiver_var
        {
            let is_mutable = ctx
                .params
                .iter()
                .find(|p| p.name == var)
                .map(|p| {
                    matches!(p.ty.kind(), TypeKind::MutRef(_))
                        || matches!(p.mode, RirParamMode::MutRef)
                })
                .or_else(|| {
                    ctx.locals
                        .get(&var)
                        .map(|local| local.is_mut || matches!(local.ty.kind(), TypeKind::MutRef(_)))
                })
                .unwrap_or(true);
            if !is_mutable {
                let name_str = self.interner.resolve(&var).to_string();
                return Err(CompileError::new(
                    ErrorKind::AssignToImmutable(name_str),
                    span,
                ));
            }
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
                RirParamMode::MutRef => {
                    if arg.mode != RirArgMode::MutRef {
                        return Err(CompileError::new(
                            ErrorKind::InoutKeywordMissing,
                            self.rir.get(args[i].value).span,
                        ));
                    }
                }
                RirParamMode::Ref => {
                    if arg.mode != RirArgMode::Ref {
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

        // ADR-0071: associated functions on the `char` primitive type.
        // `char::from_u32(n) -> Result(char, u32)` and (in `checked` blocks)
        // `char::from_u32_unchecked(n) -> char`.
        if type_name_str == "char" {
            return self.dispatch_char_assoc_fn_call(air, &function_name_str, &args, span, ctx);
        }

        // ADR-0063: `Ptr(T)::name(args)` / `MutPtr(T)::name(args)`. The RIR
        // path stores the LHS as the synthesized symbol `Ptr(T)`; sema's
        // resolve_type already handles type-call syntax via the
        // BuiltinTypeConstructor registry, so dispatch through there.
        if let Some((callee_name, _)) = crate::types::parse_type_call_syntax(&type_name_str)
            && let Some(constructor) = gruel_builtins::get_builtin_type_constructor(&callee_name)
        {
            // ADR-0066: route Vec(T)::new()/with_capacity(n) through the
            // Vec dispatcher.
            if matches!(
                constructor.kind,
                gruel_builtins::BuiltinTypeConstructorKind::Vec
            ) {
                let vec_ty = self.resolve_type(type_name, span)?;
                if let Some(result) = self.try_dispatch_vec_static_call(
                    air,
                    ctx,
                    vec_ty,
                    &function_name_str,
                    &args,
                    span,
                ) {
                    return result;
                }
            }
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
                        return Err(CompileError::type_mismatch(
                            format!(
                                "struct-style construction `{}::{} {{ ... }}`",
                                type_name_str, function_name_str
                            ),
                            format!(
                                "tuple-style construction `{}::{}(...)`",
                                type_name_str, function_name_str
                            ),
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
                            return Err(CompileError::type_mismatch(
                                field_types[i].name().to_string(),
                                result.ty.name().to_string(),
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
                        return Err(CompileError::type_mismatch(
                            method_param_types[i].name().to_string(),
                            result.ty.name().to_string(),
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

        // ADR-0066: Vec(T) static method calls via comptime type variable
        // (e.g., `let V = Vec(i32); V::new()`).
        if let Some(&ty) = ctx.comptime_type_vars.get(&type_name)
            && matches!(ty.kind(), TypeKind::Vec(_))
            && let Some(result) =
                self.try_dispatch_vec_static_call(air, ctx, ty, &function_name_str, &args, span)
        {
            return result;
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
                        return Err(CompileError::type_mismatch(
                            format!(
                                "struct-style construction `{}::{} {{ ... }}`",
                                type_name_str, function_name_str
                            ),
                            format!(
                                "tuple-style construction `{}::{}(...)`",
                                type_name_str, function_name_str
                            ),
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
                            return Err(CompileError::type_mismatch(
                                field_types[i].name().to_string(),
                                result.ty.name().to_string(),
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
                        return Err(CompileError::type_mismatch(
                            method_param_types[i].name().to_string(),
                            result.ty.name().to_string(),
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
                    return Err(CompileError::type_mismatch(
                        "struct type".to_string(),
                        ty.name().to_string(),
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

        // ADR-0081: `String::from_utf8` / `String::from_c_str` are now
        // regular associated functions on the prelude struct; they reach
        // the user-method lookup below alongside any other static method.
        // The previous registry-driven path retired with `STRING_TYPE`.

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

        // ADR-0073: gated cross-module visibility check.
        let method_is_pub = method_info.is_pub;
        let method_file_id = method_info.file_id;
        let struct_def = self.type_pool.struct_def(struct_id);
        self.check_method_visibility(
            &struct_def.name,
            struct_def.is_builtin,
            method_is_pub,
            method_file_id,
            &function_name_str,
            span,
        )?;

        // ADR-0072 + ADR-0081: the prelude `String` has a `checked`-only
        // bridge surface for static constructors (`from_utf8_unchecked`,
        // `from_c_str_unchecked`). The registry-driven dispatch is gone,
        // so apply the same name-based gates here.
        self.check_string_vec_bridge_method_gates(&struct_def.name, &function_name_str, ctx, span)?;

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
            return Err(CompileError::type_mismatch(
                "numeric type".to_string(),
                lhs_result.ty.name().to_string(),
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
        let lhs_type = lhs_result.ty;

        // Propagate Never/Error without additional type errors. Analyze rhs
        // as projection too and emit the regular `Bin` path; downstream
        // type checking is suppressed by the never/error propagation.
        if lhs_type.is_never() || lhs_type.is_error() {
            let rhs_result = self.analyze_inst_for_projection(air, rhs, ctx)?;
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Bin(op, lhs_result.air_ref, rhs_result.air_ref),
                ty: Type::BOOL,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::BOOL));
        }

        // ADR-0081 Phase 1: route `Vec(T) ==` / `<` to the Vec-method
        // dispatch path (`vec_eq` / `vec_cmp` intrinsics). The Vec receiver
        // isn't a user-declared struct, so the regular `lookup_user_method`
        // path below would miss it; we recognize it explicitly here and
        // reuse `finish_operator_dispatch` for the Ne / Lt / Le / Gt / Ge
        // wrapping. Requires `T: Copy` (enforced inside the dispatch).
        if matches!(lhs_type.kind(), TypeKind::Vec(_)) {
            let method_name = if matches!(op, BinOp::Eq | BinOp::Ne) {
                "eq"
            } else {
                "cmp"
            };
            // Pass the rhs through as a normal RIR call argument — the
            // dispatch arm will project it (no move) and verify the type.
            let dispatch_result = self.dispatch_vec_method_call(
                air,
                lhs_result,
                method_name,
                &[RirCallArg {
                    value: rhs,
                    mode: RirArgMode::Normal,
                }],
                span,
                ctx,
            )?;
            return self.finish_operator_dispatch(
                air,
                op,
                method_name,
                dispatch_result.air_ref,
                dispatch_result.ty,
                span,
            );
        }

        // ADR-0078 Phase 4: operator desugaring for non-primitive types.
        //
        // Dispatch order:
        //   1. Numeric / bool / char / unit primitives — fall through to the
        //      regular `Bin` path (existing behavior).
        //   2. User struct or enum with an `eq` (for `==` / `!=`) or `cmp`
        //      (for `<` / `<=` / `>` / `>=`) method — desugar to a method
        //      call. The conformer's signature must match `Eq::eq` /
        //      `Ord::cmp` from `prelude/cmp.gruel`. ADR-0081: the prelude
        //      `String` flows through this path; its `eq` / `cmp` methods
        //      delegate to `Vec(u8)` byte comparisons.
        //
        // This is the load-bearing piece of ADR-0078 Phase 4 — it's what
        // makes the `Eq` / `Ord` interfaces useful as overloading hooks.
        if !lhs_type.is_numeric()
            && lhs_type != Type::BOOL
            && lhs_type != Type::CHAR
            && lhs_type != Type::UNIT
        {
            // ADR-0079: read the method name out of the lang-item
            // interface declaration when available. The prelude-tagged
            // `Eq` / `Ord` interfaces each declare exactly one method,
            // so the first slot's name is the dispatch target. Falls
            // back to the historical hardcoded `"eq"` / `"cmp"` when
            // the lang item isn't bound (e.g. test fixtures that bypass
            // the prelude).
            let lang_iface_id = if matches!(op, BinOp::Eq | BinOp::Ne) {
                self.lang_items.op_eq()
            } else {
                self.lang_items.op_cmp()
            };
            let method_name_owned: String = lang_iface_id
                .and_then(|id| {
                    self.interface_defs[id.0 as usize]
                        .methods
                        .first()
                        .map(|m| m.name.clone())
                })
                .unwrap_or_else(|| {
                    if matches!(op, BinOp::Eq | BinOp::Ne) {
                        "eq".to_string()
                    } else {
                        "cmp".to_string()
                    }
                });
            let method_name: &str = &method_name_owned;
            let method_sym = self.interner.get(method_name);
            if let Some(method_sym) = method_sym
                && let Some(method_info) = self.lookup_user_method(lhs_type, method_sym)
            {
                let recv_pass_mode = match method_info.receiver {
                    crate::types::ReceiverMode::ByValue => AirArgMode::Normal,
                    crate::types::ReceiverMode::Ref => AirArgMode::Ref,
                    crate::types::ReceiverMode::MutRef => AirArgMode::MutRef,
                };
                let return_type = method_info.return_type;
                let type_name = self.format_type_name(lhs_type);

                // ADR-0026 lazy analysis: register the dispatched method so
                // its body is analyzed and emitted by the work queue. The
                // regular method-call path does this via
                // `ctx.referenced_methods.insert(method_key)`; mirror that
                // here so prelude / user-defined `eq` / `cmp` aren't dropped
                // from codegen.
                if let TypeKind::Struct(struct_id) = lhs_type.kind() {
                    ctx.referenced_methods.insert((struct_id, method_sym));
                } else if let TypeKind::Enum(enum_id) = lhs_type.kind() {
                    ctx.referenced_methods
                        .insert((crate::types::StructId(enum_id.0), method_sym));
                }

                // Analyze rhs through the regular call-arg path so it gets
                // proper move tracking against the method's `other: Self`
                // parameter. Borrow-on-projection of lhs above stays as-is.
                let rhs_args = self.analyze_call_args(
                    air,
                    &[RirCallArg {
                        value: rhs,
                        mode: RirArgMode::Normal,
                    }],
                    ctx,
                )?;
                let rhs_air_ref = rhs_args[0].value;
                let rhs_type = air.get(rhs_air_ref).ty;
                if rhs_type != lhs_type && !rhs_type.is_never() && !rhs_type.is_error() {
                    return Err(CompileError::type_mismatch(
                        type_name.clone(),
                        rhs_type.name().to_string(),
                        self.rir.get(rhs).span,
                    ));
                }

                let mut air_args = vec![AirCallArg {
                    value: lhs_result.air_ref,
                    mode: recv_pass_mode,
                }];
                air_args.extend(rhs_args);

                let call_name_str = format!("{}.{}", type_name, method_name);
                let call_name_sym = self.interner.get_or_intern(&call_name_str);

                let mut extra_data = Vec::with_capacity(air_args.len() * 2);
                for arg in &air_args {
                    extra_data.push(arg.value.as_u32());
                    extra_data.push(arg.mode.as_u32());
                }
                let args_start = air.add_extra(&extra_data);

                let call_air_ref = air.add_inst(AirInst {
                    data: AirInstData::Call {
                        name: call_name_sym,
                        args_start,
                        args_len: air_args.len() as u32,
                    },
                    ty: return_type,
                    span,
                });

                return self.finish_operator_dispatch(
                    air,
                    op,
                    method_name,
                    call_air_ref,
                    return_type,
                    span,
                );
            }
            // No `eq` / `cmp` method on this type. For ordering ops, emit a
            // helpful error naming `Ord`. For equality, fall through to the
            // existing path: structs get bitwise equality via
            // `build_value_eq`; types that aren't structs get rejected by
            // the type-validation check below.
            if !allow_bool {
                let type_name = self.format_type_name(lhs_type);
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected:
                            "a type that conforms to `Ord` (with `fn cmp(self: Ref(Self), other: Self) -> Ordering`)"
                                .to_string(),
                        found: type_name,
                    },
                    self.rir.get(lhs).span,
                )
                .with_help(format!(
                    "implement `fn cmp(self: Ref(Self), other: Self) -> Ordering` on the type to enable `{}`",
                    op.symbol()
                )));
            }
        }

        // Fall-through: analyze rhs and emit the regular `Bin` instruction.
        let rhs_result = self.analyze_inst_for_projection(air, rhs, ctx)?;

        // Validate the type is appropriate for this comparison
        if allow_bool {
            // Equality operators (==, !=) work on integers, floats, booleans, chars,
            // unit, and structs. (ADR-0071: char comparison is by codepoint
            // value.) ADR-0081: String is a regular struct in the prelude,
            // covered by `is_struct()`.
            if !lhs_type.is_numeric()
                && lhs_type != Type::BOOL
                && lhs_type != Type::CHAR
                && lhs_type != Type::UNIT
                && !lhs_type.is_struct()
            {
                return Err(CompileError::type_mismatch(
                    "numeric, bool, char, unit, or struct".to_string(),
                    lhs_type.name().to_string(),
                    self.rir.get(lhs).span,
                ));
            }
        } else if !lhs_type.is_numeric() && lhs_type != Type::CHAR {
            return Err(CompileError::type_mismatch(
                "numeric or char".to_string(),
                lhs_type.name().to_string(),
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

    /// ADR-0078 Phase 4: look up a user-defined method by name on a struct
    /// or enum type. Returns the method info if found, regardless of
    /// signature — the caller's responsibility to validate the shape.
    fn lookup_user_method(&self, ty: Type, method_sym: Spur) -> Option<MethodInfo> {
        match ty.kind() {
            TypeKind::Struct(struct_id) => self.methods.get(&(struct_id, method_sym)).cloned(),
            TypeKind::Enum(enum_id) => self.enum_methods.get(&(enum_id, method_sym)).cloned(),
            _ => None,
        }
    }

    /// ADR-0078 Phase 4: finish operator-dispatch lowering after the
    /// dispatched method call has been emitted. For `==` / `!=`, the call
    /// returned a `bool`; `!=` wraps in `Bin(Ne, call, true)`. For
    /// `<` / `<=` / `>` / `>=`, the call returned an `Ordering`; build a
    /// comparison against `Ordering::Less` or `Ordering::Greater`.
    fn finish_operator_dispatch(
        &mut self,
        air: &mut Air,
        op: BinOp,
        method_name: &str,
        call_air_ref: AirRef,
        return_type: Type,
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        match op {
            BinOp::Eq => {
                if return_type != Type::BOOL {
                    return Err(CompileError::type_mismatch(
                        "bool".to_string(),
                        return_type.name().to_string(),
                        span,
                    )
                    .with_help(format!(
                        "`fn {}(...) -> bool` is required for `Eq` conformance",
                        method_name
                    )));
                }
                Ok(AnalysisResult::new(call_air_ref, Type::BOOL))
            }
            BinOp::Ne => {
                if return_type != Type::BOOL {
                    return Err(CompileError::type_mismatch(
                        "bool".to_string(),
                        return_type.name().to_string(),
                        span,
                    )
                    .with_help(format!(
                        "`fn {}(...) -> bool` is required for `Eq` conformance",
                        method_name
                    )));
                }
                let true_ref = air.add_inst(AirInst {
                    data: AirInstData::BoolConst(true),
                    ty: Type::BOOL,
                    span,
                });
                let result = air.add_inst(AirInst {
                    data: AirInstData::Bin(BinOp::Ne, call_air_ref, true_ref),
                    ty: Type::BOOL,
                    span,
                });
                Ok(AnalysisResult::new(result, Type::BOOL))
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                // ADR-0079: prefer the lang-item binding; fall back to
                // the legacy name cache for compilations that bypass
                // the prelude entirely.
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
                let expected_ty = Type::new_enum(ordering_id);
                if return_type != expected_ty {
                    return Err(CompileError::type_mismatch(
                        "Ordering".to_string(),
                        return_type.name().to_string(),
                        span,
                    )
                    .with_help(format!(
                        "`fn {}(...) -> Ordering` is required for `Ord` conformance",
                        method_name
                    )));
                }
                // Variant indices match `prelude/cmp.gruel`:
                // Less = 0, Equal = 1, Greater = 2.
                let (variant_index, cmp_op) = match op {
                    BinOp::Lt => (0u32, BinOp::Eq), // result == Less
                    BinOp::Ge => (0u32, BinOp::Ne), // result != Less
                    BinOp::Gt => (2u32, BinOp::Eq), // result == Greater
                    BinOp::Le => (2u32, BinOp::Ne), // result != Greater
                    _ => unreachable!(),
                };
                let variant_air = air.add_inst(AirInst {
                    data: AirInstData::EnumVariant {
                        enum_id: ordering_id,
                        variant_index,
                    },
                    ty: expected_ty,
                    span,
                });
                let result = air.add_inst(AirInst {
                    data: AirInstData::Bin(cmp_op, call_air_ref, variant_air),
                    ty: Type::BOOL,
                    span,
                });
                Ok(AnalysisResult::new(result, Type::BOOL))
            }
            _ => unreachable!("operator dispatch only handles comparison ops"),
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
            if arg.mode == RirArgMode::MutRef {
                if self.extract_root_variable(arg.value).is_none() {
                    return Err(CompileError::new(
                        ErrorKind::InoutNonLvalue,
                        self.rir.get(arg.value).span,
                    ));
                }
            } else if arg.mode == RirArgMode::Ref && self.extract_root_variable(arg.value).is_none()
            {
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
        if arg.mode == RirArgMode::MutRef {
            return (true, false);
        }
        if arg.mode == RirArgMode::Ref {
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
    pub(super) fn register_anon_struct_methods_for_comptime_with_subst(
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

        // ADR-0076: bind `Self` to the anonymous struct's `Type` while
        // resolving method signatures.
        let saved_self = self.current_self.replace(struct_type);

        for method_ref in method_refs {
            let method_inst = self.rir.get(method_ref);
            if let InstData::FnDecl {
                name: method_name,
                is_pub: method_is_pub,
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
                    let resolved_ty = if p.is_comptime && p.ty == type_sym {
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

                let ret_type = if references_method_type_param(self, *return_type) {
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
                        is_pub: *method_is_pub,
                        file_id: method_inst.span.file_id,
                    },
                );
            }
        }
        self.current_self = saved_self;
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

        // ADR-0076: bind `Self` to the anonymous struct's `Type` while
        // resolving the synthesized `__call` method signature.
        let saved_self = self.current_self.replace(struct_type);

        let params = self.rir.get_params(params_start, params_len);
        let param_names: Vec<Spur> = params.iter().map(|p| p.name).collect();
        let mut param_types: Vec<Type> = Vec::with_capacity(params.len());
        for p in params {
            let resolved = self.resolve_type(p.ty, span)?;
            param_types.push(resolved);
        }

        let ret_ty = self.resolve_type(return_type, span)?;

        self.current_self = saved_self;

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
                // Synthetic `__call` is part of the lambda's surface — pub
                // so it can be invoked at any call site.
                is_pub: true,
                file_id: method_inst.span.file_id,
            },
        );
        Ok(())
    }

    /// Register methods for an anonymous enum created via comptime with type substitution.
    ///
    /// Analogous to `register_anon_struct_methods_for_comptime_with_subst`, but for enums.
    /// Resolves method parameter/return types with `Self` mapped to the anonymous enum type.
    pub(super) fn register_anon_enum_methods_for_comptime_with_subst(
        &mut self,
        enum_id: EnumId,
        enum_type: crate::types::Type,
        methods_start: u32,
        methods_len: u32,
        type_subst: &rustc_hash::FxHashMap<Spur, Type>,
    ) -> Option<()> {
        let method_refs = self.rir.get_inst_refs(methods_start, methods_len);

        let mut seen_methods: rustc_hash::FxHashSet<Spur> = rustc_hash::FxHashSet::default();

        // ADR-0076: bind `Self` to the anonymous enum's `Type` while
        // resolving method signatures.
        let saved_self = self.current_self.replace(enum_type);

        for method_ref in method_refs {
            let method_inst = self.rir.get(method_ref);
            if let InstData::FnDecl {
                name: method_name,
                is_pub: method_is_pub,
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
                    let resolved_ty =
                        self.resolve_type_for_comptime_with_subst(p.ty, type_subst)?;
                    param_types.push(resolved_ty);
                }

                let ret_type =
                    self.resolve_type_for_comptime_with_subst(*return_type, type_subst)?;

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
                        is_pub: *method_is_pub,
                        file_id: method_inst.span.file_id,
                    },
                );
            }
        }
        self.current_self = saved_self;
        Some(())
    }

    /// Extract method signatures from RIR for structural equality comparison.
    ///
    /// This extracts method signatures as type symbols (Spur), not resolved Types.
    /// This is intentional: for structural equality, we compare type symbols directly
    /// so that `Self` matches `Self` even before we know the concrete StructId.
    pub(super) fn extract_anon_method_sigs(
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
}

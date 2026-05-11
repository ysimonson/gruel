//! Semantic analysis - RIR to AIR conversion.
//!
//! Sema performs type checking and converts untyped RIR to typed AIR.
//! This is analogous to Zig's Sema phase.
//!
//! # Module Organization
//!
//! This module is split into several submodules for maintainability:
//!
//! - [`context`] - Analysis context and helper types (LocalVar, AnalysisContext, etc.)
//! - [`declarations`] - Declaration gathering (register_type_names, resolve_declarations)
//! - [`builtins`] - Built-in type injection (String, etc.)
//! - [`typeck`] - Type resolution and checking helpers
//! - [`analysis`] - Function analysis and type inference coordination
//! - [`airgen`] - RIR instruction to AIR instruction lowering
//! - [`info`] - Function, method, and constant info types
//! - [`gather`] - Declaration gathering output
//! - [`output`] - Semantic analysis output types
//! - [`inference_ctx`] - Pre-computed type information for inference
//! - [`visibility`] - Module visibility checking
//! - [`imports`] - Import resolution and const evaluation
//! - [`anon_structs`] - Anonymous struct structural equality
//! - [`anon_enums`] - Anonymous enum structural equality
//! - [`sema_ctx_builder`] - SemaContext builder for parallel analysis
//! - [`file_paths`] - File path management for multi-file compilation
//!
//! The main entry points are:
//! - [`Sema::new`] - Create a new semantic analyzer
//! - [`Sema::analyze_all`] - Perform full semantic analysis
//! - [`Sema::analyze_all_bodies`] - Analyze function bodies after declarations

mod airgen;
mod analysis;
mod analyze_ops;
mod anon_enums;
mod anon_interfaces;
mod anon_structs;
mod builtins;
mod comptime;
mod conformance;
mod context;
mod declarations;
mod file_paths;
mod gather;
mod imports;
mod inference_ctx;
mod info;
mod intrinsics;
mod known_symbols;
mod lang_items;
mod module_path;
mod output;
mod pointer_ops;
mod sema_ctx_builder;
mod typeck;
mod usefulness;
mod vec_methods;
mod visibility;

// Public re-exports
pub use context::ConstValue;
pub use gather::GatherOutput;
pub use inference_ctx::InferenceContext;
pub use info::{AnonMethodSig, ConstInfo, DeriveBinding, DeriveInfo, FunctionInfo, MethodInfo};
pub use known_symbols::KnownSymbols;
pub use lang_items::LangItems;
pub use output::{AnalyzedFunction, InterfaceVtables, SemaOutput};

use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

use gruel_rir::Rir;
use gruel_util::FileId;
use gruel_util::{CompileErrors, MultiErrorResult, PreviewFeatures};
use lasso::{Spur, ThreadedRodeo};

use crate::intern_pool::TypeInternPool;
use crate::param_arena::ParamArena;
use crate::types::{EnumId, InterfaceDef, InterfaceId, StructId, Type};

use context::ComptimeHeapItem;

/// Semantic analyzer that converts RIR to AIR.
pub struct Sema<'a> {
    pub(crate) rir: &'a Rir,
    pub(crate) interner: &'a ThreadedRodeo,
    /// Function table: maps function name symbols to their info
    pub(crate) functions: HashMap<Spur, FunctionInfo>,
    /// Struct table: maps struct name symbols to their StructId
    pub(crate) structs: HashMap<Spur, StructId>,
    /// Enum table: maps enum name symbols to their EnumId
    pub(crate) enums: HashMap<Spur, EnumId>,
    /// Interface table: maps interface name symbols to InterfaceId (ADR-0056).
    pub(crate) interfaces: HashMap<Spur, InterfaceId>,
    /// Definitions for each interface. Indexed by InterfaceId.0.
    pub(crate) interface_defs: Vec<InterfaceDef>,
    /// Interface bounds on comptime type parameters (ADR-0056 Phase 3).
    ///
    /// Keyed by `(owner, param_name)` where `owner` is either a function
    /// name (top-level functions) or a `"StructName.method"` interned spur
    /// (methods). Looked up at specialization time to drive `check_conforms`.
    pub(crate) comptime_interface_bounds: HashMap<(Spur, Spur), InterfaceId>,
    /// (StructId, InterfaceId) pairs that need a vtable emitted (ADR-0056
    /// Phase 4d). Populated by sema when a runtime coercion is detected;
    /// consumed by codegen to emit one `@__vtable__C__I` global per pair.
    /// The value is the conformance witness — the conforming type's method
    /// keys in interface declaration order, ready for codegen to look up the
    /// LLVM function.
    pub(crate) interface_vtables_needed: InterfaceVtables,
    /// Method table: maps (struct_id, method_name) to method info
    pub(crate) methods: HashMap<(StructId, Spur), MethodInfo>,
    /// Enum method table: maps (enum_id, method_name) to method info
    pub(crate) enum_methods: HashMap<(EnumId, Spur), MethodInfo>,
    /// Derive table: maps a derive name to its method-template info
    /// (ADR-0058). Populated during declaration gathering; consumed by
    /// `@derive(...)` expansion.
    pub(crate) derives: HashMap<Spur, DeriveInfo>,
    /// Pending `@derive(D)` bindings on named struct/enum declarations
    /// (ADR-0058). Populated during directive resolution; consumed by the
    /// derive-expansion sub-phase.
    pub(crate) derive_bindings: Vec<DeriveBinding>,
    /// Errors raised during anonymous-host derive expansion (ADR-0058).
    /// The comptime interpreter returns `Option<...>` so it cannot
    /// propagate these via `?`; we buffer them here and surface after
    /// analysis so users still see actionable diagnostics for an
    /// `@derive(...)` error on an anonymous struct/enum.
    pub(crate) pending_anon_derive_errors: Vec<gruel_util::CompileError>,
    /// Validation errors raised while evaluating an anonymous struct/enum
    /// type literal at comptime (empty body, duplicate method names).
    /// Buffered for the same `Option<...>` reason as `pending_anon_derive_errors`.
    /// `evaluate_type_ctor_body` drains the entries it caused so the call site
    /// surfaces the specific error instead of a generic "comptime evaluation
    /// failed"; any leftover entries are surfaced by `analyze_all` at the end.
    pub(crate) pending_anon_eval_errors: Vec<gruel_util::CompileError>,
    /// Constant table: maps const name symbol to const info
    pub(crate) constants: HashMap<Spur, ConstInfo>,
    /// Enabled preview features
    pub(crate) preview_features: PreviewFeatures,
    /// StructId of the synthetic String type.
    pub(crate) builtin_string_id: Option<StructId>,
    /// EnumId of the synthetic Arch enum (for @target_arch intrinsic).
    pub(crate) builtin_arch_id: Option<EnumId>,
    /// EnumId of the synthetic Os enum (for @target_os intrinsic).
    pub(crate) builtin_os_id: Option<EnumId>,
    /// EnumId of the synthetic TypeKind enum (for @type_info intrinsic).
    pub(crate) builtin_typekind_id: Option<EnumId>,
    /// EnumId of the synthetic Ownership enum (for @ownership intrinsic).
    pub(crate) builtin_ownership_id: Option<EnumId>,
    /// EnumId of the prelude `ThreadSafety` enum (ADR-0084), used by
    /// the `@thread_safety` intrinsic to materialize a value of the
    /// classification ladder.
    pub(crate) builtin_thread_safety_id: Option<EnumId>,
    /// EnumId of the prelude `Ordering` enum (ADR-0078 Phase 4: target of
    /// `Ord::cmp`; analyzed at every `<`/`<=`/`>`/`>=` desugaring on a
    /// type that conforms to `Ord`).
    pub(crate) builtin_ordering_id: Option<EnumId>,
    /// ADR-0079: lang-item registry. Populated from `@lang("…")`
    /// directives on prelude declarations; the compiler keys
    /// drop/copy/clone/handle/Eq/Ord/Ordering behaviors off these IDs
    /// instead of the historical name-string match.
    pub(crate) lang_items: LangItems,
    /// Pre-interned known symbols for fast comparison.
    pub(crate) known: KnownSymbols,
    /// Type intern pool for unified type representation (ADR-0024 Phase 1).
    pub(crate) type_pool: TypeInternPool,
    /// Module registry for tracking imported modules (Phase 1 modules).
    pub(crate) module_registry: crate::sema_context::ModuleRegistry,
    /// Maps FileId to source file paths (for module resolution).
    pub(crate) file_paths: HashMap<FileId, String>,
    /// Arena storage for function/method parameter data.
    pub(crate) param_arena: ParamArena,
    /// Inline destructor bodies keyed by struct id (ADR-0053).
    ///
    /// Populated when a struct body contains `fn __drop(self)`. The analysis pass
    /// looks these up to run `analyze_destructor_function` against the method
    /// body.
    pub(crate) inline_struct_drops: HashMap<StructId, (gruel_rir::InstRef, gruel_util::Span)>,
    /// Inline destructor bodies keyed by enum id (ADR-0053 phase 3b).
    /// Same contract as `inline_struct_drops` but for enums.
    pub(crate) inline_enum_drops: HashMap<EnumId, (gruel_rir::InstRef, gruel_util::Span)>,
    /// Method signatures for anonymous structs, used for structural equality comparison.
    pub(crate) anon_struct_method_sigs: HashMap<StructId, Vec<AnonMethodSig>>,
    /// Captured comptime values for anonymous structs.
    /// When an anonymous struct with methods is created inside a comptime function,
    /// the comptime parameter values (e.g., N=42 in FixedBuffer(comptime N: i32)) are
    /// stored here, keyed by StructId. These values become part of type identity:
    /// FixedBuffer(42) and FixedBuffer(100) are different types.
    pub(crate) anon_struct_captured_values: HashMap<StructId, HashMap<Spur, ConstValue>>,
    /// ADR-0082: captured comptime *type* substitutions for anonymous
    /// structs created from a parameterized comptime function (e.g.
    /// `pub fn Vec(comptime T: type) -> type { struct { ... } }`). Stores
    /// `T → I32` per `Vec(I32)` instance. Looked up when analyzing method
    /// bodies so type names that reference the outer fn's comptime params
    /// resolve at body-analysis time. Parallels `anon_struct_captured_values`
    /// for type substitutions instead of value substitutions.
    pub(crate) anon_struct_type_subst: HashMap<StructId, HashMap<Spur, Type>>,
    /// Method signatures for anonymous enums, used for structural equality comparison.
    pub(crate) anon_enum_method_sigs: HashMap<EnumId, Vec<AnonMethodSig>>,
    /// Captured comptime values for anonymous enums (same semantics as anonymous structs).
    pub(crate) anon_enum_captured_values: HashMap<EnumId, HashMap<Spur, ConstValue>>,
    /// ADR-0082: captured comptime *type* substitutions for anonymous
    /// enums (parallel to `anon_struct_type_subst`).
    pub(crate) anon_enum_type_subst: HashMap<EnumId, HashMap<Spur, Type>>,
    /// ADR-0082: registry of `StructId`s produced by instantiating the
    /// `@lang("vec")` function for some element type `T`. Maps the
    /// instance struct's `StructId` to the element type. Populated when
    /// `Vec(T)` is evaluated in type position; consulted by the
    /// `as_vec_instance` helper used by indexing, slice borrow, drop
    /// synthesis, and method dispatch.
    pub(crate) vec_instance_registry: HashMap<StructId, Type>,
    /// ADR-0082: the prelude `@lang(...)` function whose body is
    /// currently being evaluated by the comptime interpreter. Set
    /// transiently by callers of `try_evaluate_const_with_subst` /
    /// `evaluate_type_ctor_body` so the anon-struct evaluation path
    /// can detect "this struct is a Vec instance" and populate
    /// `vec_instance_registry`. `None` outside such evaluations.
    pub(crate) comptime_ctor_fn: Option<Spur>,
    /// Loop iteration counter for the current comptime block evaluation.
    /// Reset to 0 at the start of each `evaluate_comptime_block` call.
    /// Incremented once per loop iteration; triggers an error when it exceeds
    /// `COMPTIME_MAX_STEPS` to prevent infinite loops at compile time.
    pub(crate) comptime_steps_used: u64,
    /// Pending return value for the comptime interpreter.
    /// Set by `Ret` instructions inside comptime function bodies; consumed
    /// immediately by the enclosing `Call` handler in `evaluate_comptime_inst`.
    pub(crate) comptime_return_value: Option<ConstValue>,
    /// Current call stack depth in the comptime interpreter.
    /// Incremented on each comptime `Call`, decremented on return.
    /// Triggers an error if it exceeds `COMPTIME_CALL_DEPTH_LIMIT`.
    pub(crate) comptime_call_depth: u32,
    /// Comptime heap: stores composite values (structs, arrays) created during
    /// comptime evaluation. `ConstValue::Struct(idx)` and `ConstValue::Array(idx)`
    /// index into this vec. Cleared at the start of each `evaluate_comptime_block`.
    pub(crate) comptime_heap: Vec<ComptimeHeapItem>,
    /// Type overrides for the comptime interpreter during generic function calls.
    /// When a comptime generic call is executing, type parameters are stored here
    /// so that enum/struct resolution can find them. Checked before `ctx.comptime_type_vars`.
    pub(crate) comptime_type_overrides: HashMap<Spur, Type>,
    /// Buffer for `@dbg` output collected during comptime evaluation.
    /// Each entry is one formatted line (without trailing newline), matching
    /// the format of the runtime `__gruel_dbg_*` functions.
    pub(crate) comptime_dbg_output: Vec<String>,
    /// Pending warnings for comptime `@dbg` calls. Each entry is (message, span).
    pub(crate) comptime_log_output: Vec<(String, gruel_util::Span)>,
    /// When true, comptime `@dbg` does not print to stderr on-the-fly. The output
    /// is still appended to `comptime_dbg_output` and a warning is still emitted.
    /// Set by the `--capture-comptime-dbg` CLI flag (used by the fuzzer).
    pub(crate) suppress_comptime_dbg_print: bool,
    /// ADR-0076: the in-scope `Self` type, set whenever we resolve types
    /// inside a struct/enum body (its methods or its inline destructor),
    /// inside a `derive` splice into a host type, or inside the body of a
    /// method that was synthesized for a comptime-built anonymous type.
    /// Consumed by `resolve_type` / `resolve_type_for_comptime_with_subst`
    /// to substitute the literal symbol `Self` at any depth in a type
    /// expression. `None` means `Self` is not in scope; using it is an
    /// error.
    pub(crate) current_self: Option<Type>,
    /// The compilation target. Read by `@target_arch()` / `@target_os()`
    /// so conditional code reflects the *compile* target, not the host.
    /// Defaults to the host target; the driver overrides via
    /// [`Sema::set_target`] when a different `--target` is requested.
    pub(crate) target: gruel_target::Target,
    /// ADR-0083: names of struct/enum declarations that carry
    /// `@mark(affine)`. Affine is a Copy *suppressor*: a type whose
    /// members would otherwise infer Copy is forced to remain Affine.
    /// Tracked here as a side set because `StructDef.posture =
    /// Posture::Affine` is the same shape "no declaration" produces.
    pub(crate) mark_affine_decls: HashSet<Spur>,

    /// ADR-0084: names of struct/enum declarations that carry one of
    /// the thread-safety override markers (`@mark(unsend)` /
    /// `@mark(checked_send)` / `@mark(checked_sync)`). Tracked as a
    /// side map so `validate_consistency` can apply the override after
    /// computing the structural minimum, mirroring the
    /// `mark_affine_decls` carve-out for `@mark(affine)`.
    pub(crate) mark_thread_safety_decls: rustc_hash::FxHashMap<Spur, gruel_builtins::ThreadSafety>,
}

impl<'a> Sema<'a> {
    /// Create a new semantic analyzer.
    pub fn new(
        rir: &'a Rir,
        interner: &'a ThreadedRodeo,
        preview_features: PreviewFeatures,
    ) -> Self {
        let type_pool = TypeInternPool::new();
        Self {
            rir,
            interner,
            functions: HashMap::default(),
            structs: HashMap::default(),
            enums: HashMap::default(),
            interfaces: HashMap::default(),
            interface_defs: Vec::new(),
            comptime_interface_bounds: HashMap::default(),
            interface_vtables_needed: HashMap::default(),
            methods: HashMap::default(),
            enum_methods: HashMap::default(),
            derives: HashMap::default(),
            derive_bindings: Vec::new(),
            pending_anon_derive_errors: Vec::new(),
            pending_anon_eval_errors: Vec::new(),
            constants: HashMap::default(),
            preview_features,
            builtin_string_id: None,
            builtin_arch_id: None,
            builtin_os_id: None,
            builtin_typekind_id: None,
            builtin_ownership_id: None,
            builtin_thread_safety_id: None,
            builtin_ordering_id: None,
            lang_items: LangItems::default(),
            known: KnownSymbols::new(interner),
            type_pool,
            module_registry: crate::sema_context::ModuleRegistry::new(),
            file_paths: HashMap::default(),
            param_arena: ParamArena::new(),
            inline_struct_drops: HashMap::default(),
            inline_enum_drops: HashMap::default(),
            anon_struct_method_sigs: HashMap::default(),
            anon_struct_captured_values: HashMap::default(),
            anon_struct_type_subst: HashMap::default(),
            anon_enum_method_sigs: HashMap::default(),
            anon_enum_captured_values: HashMap::default(),
            anon_enum_type_subst: HashMap::default(),
            vec_instance_registry: HashMap::default(),
            comptime_ctor_fn: None,
            comptime_steps_used: 0,
            comptime_return_value: None,
            comptime_call_depth: 0,
            comptime_heap: Vec::new(),
            comptime_type_overrides: HashMap::default(),
            comptime_dbg_output: Vec::new(),
            comptime_log_output: Vec::new(),
            suppress_comptime_dbg_print: false,
            current_self: None,
            target: gruel_target::Target::host(),
            mark_affine_decls: HashSet::default(),
            mark_thread_safety_decls: rustc_hash::FxHashMap::default(),
        }
    }

    /// Override the compile target read by `@target_arch()` and
    /// `@target_os()`. Defaults to [`gruel_target::Target::host()`].
    pub fn set_target(&mut self, target: gruel_target::Target) {
        self.target = target;
    }

    /// Configure whether comptime `@dbg` prints to stderr on-the-fly.
    /// When suppressed, output is still buffered into `comptime_dbg_output`
    /// and warnings are still emitted.
    pub fn set_suppress_comptime_dbg_print(&mut self, suppress: bool) {
        self.suppress_comptime_dbg_print = suppress;
    }

    /// Perform semantic analysis on the RIR.
    ///
    /// This is the main entry point for semantic analysis. It returns analyzed
    /// functions, struct definitions, enum definitions, and any warnings.
    pub fn analyze_all(mut self) -> MultiErrorResult<SemaOutput> {
        // Phase 0: Inject built-in types (String, etc.) before user code
        self.inject_builtin_types();

        // Phase 1: Register type names
        // Phase 2: Resolve all declarations (this also validates interface
        // declarations between struct/enum field resolution and function
        // gathering — ADR-0056).
        self.register_type_names().map_err(CompileErrors::from)?;
        self.resolve_declarations().map_err(CompileErrors::from)?;

        // ADR-0078 Phase 3: cache EnumIds for the prelude-resident builtin
        // enums (Arch, Os, TypeKind, Ownership) now that the prelude has
        // been resolved. Intrinsics that produce values of these types
        // read `builtin_arch_id` etc. directly.
        self.cache_builtin_enum_ids();

        // Phase 2.5: Evaluate const initializers (e.g., const x = @import(...))
        self.evaluate_const_initializers()
            .map_err(CompileErrors::from)?;

        // Delegate to the analysis module for function body analysis
        analysis::analyze_all_function_bodies(self)
    }

    /// Analyze all function bodies, assuming declarations are already collected.
    pub fn analyze_all_bodies(self) -> MultiErrorResult<SemaOutput> {
        analysis::analyze_all_function_bodies(self)
    }
}

#[cfg(test)]
mod consistency_tests;
#[cfg(test)]
mod tests;

//! Immutable semantic analysis context.
//!
//! This module contains `SemaContext`, which holds all type information and
//! declarations that are immutable after the declaration gathering phase.
//! `SemaContext` is designed to be `Send + Sync` for parallel function analysis.
//!
//! # Architecture
//!
//! The semantic analysis pipeline is split into two phases:
//!
//! 1. **Declaration gathering** (sequential): Builds the `SemaContext` with all
//!    type definitions, function signatures, and method signatures.
//!
//! 2. **Function body analysis** (parallelizable): Each function is analyzed
//!    using a `FunctionAnalyzer` that holds a reference to the shared `SemaContext`.
//!
//! This separation enables:
//! - Parallel type checking (each function can be analyzed independently)
//! - Better cache locality (context can be shared across threads)
//! - Foundation for incremental compilation (can cache `SemaContext` across compilations)
//!
//! # Array Type Registry
//!
//! The array type registry is thread-safe to support parallel function analysis.
//! Array types can be created during function body analysis when type inference
//! resolves array literals like `[1, 2, 3]` without explicit type annotations.
//! The registry uses `RwLock` for concurrent access with the following pattern:
//! - Read lock for lookups (most common case)
//! - Write lock for insertions (rare, only for new array types)

use rustc_hash::FxHashMap as HashMap;
use std::sync::{PoisonError, RwLock};

use gruel_builtins::Posture;
use gruel_rir::Rir;
use gruel_util::PreviewFeatures;
use lasso::{Spur, ThreadedRodeo};

use crate::inference::{FunctionSig, InferType, MethodSig};
use crate::intern_pool::TypeInternPool;
use crate::param_arena::ParamArena;
// Import FunctionInfo, MethodInfo, and KnownSymbols from sema module to avoid duplication.
// FunctionInfo and MethodInfo are the canonical definitions; we re-export them for convenience.
pub use crate::sema::{FunctionInfo, KnownSymbols, MethodInfo};
use crate::types::{
    ArrayTypeId, EnumDef, EnumId, ModuleDef, ModuleId, StructDef, StructId, Type, TypeKind,
};

/// Thread-safe registry for modules.
///
/// This registry allows concurrent lookups and insertions of imported modules during
/// parallel function analysis. It uses double-checked locking to minimize contention.
#[derive(Debug)]
pub struct ModuleRegistry {
    /// Maps import path (e.g., "math.gruel") to ModuleId.
    paths: RwLock<HashMap<String, ModuleId>>,
    /// Module definitions indexed by ModuleId.
    defs: RwLock<Vec<ModuleDef>>,
}

impl ModuleRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            paths: RwLock::new(HashMap::default()),
            defs: RwLock::new(Vec::new()),
        }
    }

    /// Look up a module by import path.
    pub fn get(&self, import_path: &str) -> Option<ModuleId> {
        self.paths
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(import_path)
            .copied()
    }

    /// Get or create a module for the given import path and resolved file path.
    ///
    /// Returns the ModuleId and whether it was newly created.
    pub fn get_or_create(&self, import_path: String, file_path: String) -> (ModuleId, bool) {
        // Fast path: check if already exists
        {
            let paths = self.paths.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(id) = paths.get(&import_path) {
                return (*id, false);
            }
        }

        // Slow path: acquire write lock and insert
        let mut paths = self.paths.write().unwrap_or_else(PoisonError::into_inner);
        // Double-check after acquiring write lock
        if let Some(id) = paths.get(&import_path) {
            return (*id, false);
        }

        let mut defs = self.defs.write().unwrap_or_else(PoisonError::into_inner);
        let id = ModuleId::new(defs.len() as u32);
        defs.push(ModuleDef::new(import_path.clone(), file_path));
        paths.insert(import_path, id);
        (id, true)
    }

    /// Get a module definition by ID.
    pub fn get_def(&self, id: ModuleId) -> ModuleDef {
        self.defs
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id.index() as usize)
            .cloned()
            .expect("Invalid ModuleId")
    }

    /// Update a module definition.
    pub fn update_def(&self, id: ModuleId, def: ModuleDef) {
        let mut defs = self.defs.write().unwrap_or_else(PoisonError::into_inner);
        defs[id.index() as usize] = def;
    }

    /// Get the number of modules in the registry.
    pub fn len(&self) -> usize {
        self.defs
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Snapshot every module definition in registration order.
    pub fn all_defs(&self) -> Vec<ModuleDef> {
        self.defs
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Extract the module definitions (consumes the registry).
    pub fn into_defs(self) -> Vec<ModuleDef> {
        self.defs
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for ModuleRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Pre-computed type information for constraint generation.
///
/// This struct holds the function, struct, enum, and method signature maps
/// converted to `InferType` format for use in Hindley-Milner type inference.
/// Building this once and reusing it for all function analyses avoids the
/// O(n²) cost of rebuilding these maps for each function.
#[derive(Debug)]
pub struct InferenceContext {
    /// Function signatures with InferType (for constraint generation).
    pub func_sigs: HashMap<Spur, FunctionSig>,
    /// Struct types: name -> Type::new_struct(id).
    pub struct_types: HashMap<Spur, Type>,
    /// Enum types: name -> Type::new_enum(id).
    pub enum_types: HashMap<Spur, Type>,
    /// Method signatures with InferType: (struct_id, method_name) -> MethodSig.
    pub method_sigs: HashMap<(StructId, Spur), MethodSig>,
}

/// Context for semantic analysis, designed for parallel function analysis.
///
/// This struct contains all type information and declarations needed during
/// function body analysis. It is designed to be `Send + Sync` so it can be
/// shared across threads during parallel function analysis.
///
/// # Contents
///
/// - Struct and enum definitions (immutable)
/// - Function and method signatures (references to immutable data in Sema)
/// - Type intern pool (thread-safe, allows concurrent array interning)
/// - Pre-computed inference context (immutable)
/// - Built-in type IDs (immutable)
/// - Parameter arena for function/method parameter data (immutable after declaration gathering)
///
/// # Thread Safety
///
/// `SemaContext` is `Send + Sync` because:
/// - Most fields are immutable after construction
/// - The type intern pool uses `RwLock` for thread-safe mutations
/// - References to RIR and interner are shared immutably
/// - References to functions/methods HashMaps are immutable after declaration gathering
/// - Reference to param_arena is immutable after declaration gathering
/// - ThreadedRodeo is designed to be thread-safe
#[derive(Debug)]
pub struct SemaContext<'a> {
    /// Reference to the RIR being analyzed.
    pub rir: &'a Rir,
    /// Reference to the string interner.
    pub interner: &'a ThreadedRodeo,
    /// Struct lookup: maps struct name symbol to StructId.
    pub structs: HashMap<Spur, StructId>,
    /// Enum lookup: maps enum name symbol to EnumId.
    pub enums: HashMap<Spur, EnumId>,
    /// Function lookup: reference to Sema's function map (immutable after declaration gathering).
    pub functions: &'a HashMap<Spur, FunctionInfo>,
    /// Method lookup: reference to Sema's method map (immutable after declaration gathering).
    /// Uses (StructId, method_name) key to support anonymous struct methods.
    pub methods: &'a HashMap<(StructId, Spur), MethodInfo>,
    /// Enabled preview features.
    pub preview_features: PreviewFeatures,
    /// StructId of the synthetic String type.
    pub builtin_string_id: Option<StructId>,
    /// EnumId of the synthetic Arch enum (for @target_arch intrinsic).
    pub builtin_arch_id: Option<EnumId>,
    /// EnumId of the synthetic Os enum (for @target_os intrinsic).
    pub builtin_os_id: Option<EnumId>,
    /// EnumId of the synthetic TypeKind enum (for @type_info intrinsic).
    pub builtin_typekind_id: Option<EnumId>,
    /// EnumId of the synthetic Ownership enum (for @ownership intrinsic).
    pub builtin_ownership_id: Option<EnumId>,
    /// EnumId of the prelude `ThreadSafety` enum (ADR-0084).
    pub builtin_thread_safety_id: Option<EnumId>,
    /// EnumId of the prelude `Ordering` enum (ADR-0078 Phase 4: target of
    /// `Ord::cmp`).
    pub builtin_ordering_id: Option<EnumId>,
    /// Compilation target (architecture + OS).
    pub target: gruel_target::Target,
    /// Pre-computed inference context for HM type inference.
    pub inference_ctx: InferenceContext,
    /// Pre-interned known symbols for fast comparison.
    pub known: KnownSymbols,
    /// Type intern pool for unified type representation (ADR-0024 Phase 1).
    ///
    /// During Phase 1, the pool coexists with the existing type registries.
    /// It can be used for lookups but the canonical type representation
    /// remains the old `Type` enum. Later phases will migrate to using
    /// the pool exclusively.
    pub type_pool: TypeInternPool,
    /// Thread-safe module registry.
    /// Supports concurrent lookups and insertions during parallel analysis.
    pub module_registry: ModuleRegistry,
    /// Path to the current source file being compiled (single-file mode).
    /// Used for resolving relative imports when only one file is compiled.
    pub source_file_path: Option<String>,
    /// Maps FileId to source file paths (multi-file mode).
    /// Used for resolving relative imports when multiple files are compiled.
    pub file_paths: HashMap<gruel_util::FileId, String>,
    /// Reference to the parameter arena for accessing function/method parameter data.
    /// Use `param_arena.types(fn_info.params)` to get parameter types, etc.
    pub param_arena: &'a ParamArena,
    /// Constant lookup: reference to Sema's constant map (immutable after declaration gathering).
    /// Used for looking up const declarations like `const x = @import("...")`.
    pub constants: &'a HashMap<Spur, crate::sema::ConstInfo>,
}

// SAFETY: SemaContext is Send + Sync because:
// - Immutable fields (structs, enums, etc.) are trivially thread-safe
// - ModuleRegistry uses RwLock for interior mutability
// - TypeInternPool uses RwLock for interior mutability (including array interning)
// - References to RIR and ThreadedRodeo are shared immutably
// - References to functions/methods HashMaps are shared immutably (read-only after declaration gathering)
// - ThreadedRodeo is designed to be thread-safe
// - &HashMap<K, V> is Send + Sync when the HashMap is (immutable references are always safe)
unsafe impl<'a> Send for SemaContext<'a> {}
unsafe impl<'a> Sync for SemaContext<'a> {}

impl<'a> SemaContext<'a> {
    /// Get the prelude `String` type as a `Type::Struct`. Returns
    /// `Type::ERROR` when the prelude isn't loaded (e.g., test fixtures
    /// that bypass the prelude); callers propagate the error cleanly.
    pub fn builtin_string_type(&self) -> Type {
        self.builtin_string_id
            .map(Type::new_struct)
            .unwrap_or(Type::ERROR)
    }

    /// Look up a struct by name.
    pub fn get_struct(&self, name: Spur) -> Option<StructId> {
        self.structs.get(&name).copied()
    }

    /// Get a struct definition by ID.
    pub fn get_struct_def(&self, id: StructId) -> StructDef {
        self.type_pool.struct_def(id)
    }

    /// Look up an enum by name.
    pub fn get_enum(&self, name: Spur) -> Option<EnumId> {
        self.enums.get(&name).copied()
    }

    /// Get an enum definition by ID.
    pub fn get_enum_def(&self, id: EnumId) -> EnumDef {
        self.type_pool.enum_def(id)
    }

    /// Look up a function by name.
    pub fn get_function(&self, name: Spur) -> Option<&FunctionInfo> {
        self.functions.get(&name)
    }

    /// Look up a method by struct ID and method name.
    pub fn get_method(&self, struct_id: StructId, method_name: Spur) -> Option<&MethodInfo> {
        self.methods.get(&(struct_id, method_name))
    }

    /// Look up a constant by name.
    pub fn get_constant(&self, name: Spur) -> Option<&crate::sema::ConstInfo> {
        self.constants.get(&name)
    }

    /// Get an array type definition by ID.
    ///
    /// Returns `(element_type, length)` for the array.
    pub fn get_array_type_def(&self, id: ArrayTypeId) -> (Type, u64) {
        self.type_pool.array_def(id)
    }

    /// Look up an array type by element type and length.
    pub fn get_array_type(&self, element_type: Type, length: u64) -> Option<ArrayTypeId> {
        self.type_pool.get_array_by_type(element_type, length)
    }

    /// Get or create an array type. Thread-safe.
    pub fn get_or_create_array_type(&self, element_type: Type, length: u64) -> ArrayTypeId {
        self.type_pool.intern_array_from_type(element_type, length)
    }

    /// Get or create a ptr const type. Thread-safe.
    pub fn get_or_create_ptr_const_type(&self, pointee_type: Type) -> crate::types::PtrConstTypeId {
        self.type_pool.intern_ptr_const_from_type(pointee_type)
    }

    /// Get or create a ptr mut type. Thread-safe.
    pub fn get_or_create_ptr_mut_type(&self, pointee_type: Type) -> crate::types::PtrMutTypeId {
        self.type_pool.intern_ptr_mut_from_type(pointee_type)
    }

    /// Look up a module by import path.
    pub fn get_module(&self, import_path: &str) -> Option<ModuleId> {
        self.module_registry.get(import_path)
    }

    /// Get a module definition by ID.
    pub fn get_module_def(&self, id: ModuleId) -> ModuleDef {
        self.module_registry.get_def(id)
    }

    /// Get or create a module for the given import path and file path. Thread-safe.
    ///
    /// Returns the ModuleId and whether it was newly created.
    pub fn get_or_create_module(&self, import_path: String, file_path: String) -> (ModuleId, bool) {
        self.module_registry.get_or_create(import_path, file_path)
    }

    /// Update a module definition with populated declarations.
    pub fn update_module_def(&self, id: ModuleId, def: ModuleDef) {
        self.module_registry.update_def(id, def);
    }

    /// Get the source file path for a span.
    ///
    /// Looks up the file path using the span's file_id. Falls back to
    /// `source_file_path` for single-file compilation mode.
    pub fn get_source_path(&self, span: gruel_util::Span) -> Option<&str> {
        // First, try the file_paths map (multi-file mode)
        if let Some(path) = self.file_paths.get(&span.file_id) {
            return Some(path.as_str());
        }
        // Fall back to source_file_path (single-file mode)
        self.source_file_path.as_deref()
    }

    /// Get the file path for a given FileId.
    pub fn get_file_path(&self, file_id: gruel_util::FileId) -> Option<&str> {
        self.file_paths.get(&file_id).map(|s| s.as_str())
    }

    /// Check if the accessing file can see a private item from the target file.
    ///
    /// Visibility rules (per ADR-0026):
    /// - `pub` items are always accessible
    /// - Private items are accessible if the files are in the same directory module
    ///
    /// Directory module membership includes:
    /// - Files directly in the directory (e.g., `utils/strings.gruel` is in `utils`)
    /// - Facade files for the directory (e.g., `_utils.gruel` is in `utils` module)
    ///
    /// Returns true if the item is accessible.
    pub fn is_accessible(
        &self,
        accessing_file_id: gruel_util::FileId,
        target_file_id: gruel_util::FileId,
        is_pub: bool,
    ) -> bool {
        // Public items are always accessible
        if is_pub {
            return true;
        }

        // Get paths for both files
        let accessing_path = self.get_file_path(accessing_file_id);
        let target_path = self.get_file_path(target_file_id);

        // If we can't determine the paths, be permissive (for single-file mode or tests)
        match (accessing_path, target_path) {
            (Some(acc), Some(tgt)) => {
                use std::path::Path;

                // Get the "module identity" for each file.
                // For a regular file like `utils/strings.gruel`, the module is `utils/`
                // For a facade file like `_utils.gruel`, the module is `utils/` (the directory it represents)
                let acc_module = Self::get_module_identity(Path::new(acc));
                let tgt_module = Self::get_module_identity(Path::new(tgt));

                acc_module == tgt_module
            }
            // If either path is unknown, allow access (e.g., synthetic types, single-file mode)
            _ => true,
        }
    }

    /// Get the module identity for a file path.
    ///
    /// - For regular files: returns the parent directory
    /// - For facade files (`_foo.gruel`): returns the corresponding directory (`foo/`)
    ///
    /// This allows facade files to be treated as part of their corresponding directory module.
    fn get_module_identity(path: &std::path::Path) -> Option<std::path::PathBuf> {
        let parent = path.parent()?;
        let file_stem = path.file_stem()?.to_str()?;

        // Check if this is a facade file (starts with underscore)
        if let Some(module_name) = file_stem.strip_prefix('_') {
            // Facade file: _utils.gruel -> parent/utils
            // Strip the leading underscore
            Some(parent.join(module_name))
        } else {
            // Regular file: the module is just the parent directory
            Some(parent.to_path_buf())
        }
    }

    /// Get a human-readable name for a type.
    pub fn format_type_name(&self, ty: Type) -> String {
        self.type_pool.format_type_name(ty)
    }

    /// Check if a type is a Copy type.
    pub fn is_type_copy(&self, ty: Type) -> bool {
        match ty.kind() {
            // Primitive Copy types
            TypeKind::I8
            | TypeKind::I16
            | TypeKind::I32
            | TypeKind::I64
            | TypeKind::U8
            | TypeKind::U16
            | TypeKind::U32
            | TypeKind::U64
            | TypeKind::Isize
            | TypeKind::Usize
            | TypeKind::F16
            | TypeKind::F32
            | TypeKind::F64
            | TypeKind::Bool
            | TypeKind::Char
            | TypeKind::Unit => true,
            // Enum types are Copy (they're small discriminant values), unless
            // any payload is linear (ADR-0067).
            TypeKind::Enum(enum_id) => {
                let def = self.type_pool.enum_def(enum_id);
                !def.variants
                    .iter()
                    .any(|v| v.fields.iter().any(|f| self.is_type_linear(*f)))
            }
            // Never, Error, ComptimeType, ComptimeStr, and ComptimeInt are Copy for convenience
            TypeKind::Never
            | TypeKind::Error
            | TypeKind::ComptimeType
            | TypeKind::ComptimeStr
            | TypeKind::ComptimeInt => true,
            // Struct types: check the declared/inferred posture.
            TypeKind::Struct(struct_id) => {
                let struct_def = self.type_pool.struct_def(struct_id);
                struct_def.posture == Posture::Copy
            }
            // Arrays are Copy if their element type is Copy
            TypeKind::Array(array_id) => {
                let (element_type, _length) = self.type_pool.array_def(array_id);
                self.is_type_copy(element_type)
            }
            // Module types are Copy (they're just compile-time namespace references)
            TypeKind::Module(_) => true,
            // Pointer types are Copy (they're just addresses)
            TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => true,
            // References (ADR-0062) are Copy — see Sema::is_type_copy.
            TypeKind::Ref(_) | TypeKind::MutRef(_) => true,
            // Interface types: see Sema::is_type_copy. The fat pointer is
            // bitwise-Copy.
            TypeKind::Interface(_) => true,
            // Slices (ADR-0064) are Copy — scope-bound fat pointers.
            TypeKind::Slice(_) | TypeKind::MutSlice(_) => true,
            // Vec(T) (ADR-0066) is affine — owns heap memory.
            TypeKind::Vec(_) => false,
        }
    }

    /// Get the number of ABI slots required for a type.
    pub fn abi_slot_count(&self, ty: Type) -> u32 {
        self.type_pool.abi_slot_count(ty)
    }

    /// Get the slot offset of a field within a struct.
    pub fn field_slot_offset(&self, struct_id: StructId, field_index: usize) -> u32 {
        let struct_def = self.type_pool.struct_def(struct_id);
        struct_def.fields[..field_index]
            .iter()
            .map(|f| self.abi_slot_count(f.ty))
            .sum()
    }

    /// Convert a concrete Type to InferType for use in constraint generation.
    pub fn type_to_infer_type(&self, ty: Type) -> InferType {
        match ty.kind() {
            TypeKind::Array(array_id) => {
                let (element_type, length) = self.type_pool.array_def(array_id);
                let element_infer = self.type_to_infer_type(element_type);
                InferType::Array {
                    element: Box::new(element_infer),
                    length,
                }
            }
            // ComptimeInt coerces to any integer type (like an integer literal)
            TypeKind::ComptimeInt => InferType::IntLiteral,
            _ => InferType::Concrete(ty),
        }
    }

    // ========================================================================
    // Builtin type helpers (duplicated from Sema for parallel analysis)
    // ========================================================================

    /// Check if a type is the prelude `String` struct.
    pub fn is_builtin_string(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Struct(struct_id) => Some(struct_id) == self.builtin_string_id,
            _ => false,
        }
    }

    /// Check if a type is a linear type.
    ///
    /// Delegates to `TypeInternPool::is_type_linear`, which is the single
    /// source of truth for linearity semantics (ADR-0067).
    pub fn is_type_linear(&self, ty: Type) -> bool {
        self.type_pool.is_type_linear(ty)
    }

    /// Check if a type conforms to the `Clone` interface (ADR-0065).
    ///
    /// Linear types never conform. Copy types automatically conform.
    /// `@derive(Clone)` structs (with `is_clone == true`) conform via the
    /// synthesized `<TypeName>.clone`. User structs with hand-written
    /// `fn clone(borrow self) -> Self` need full conformance check via
    /// `check_conforms`; this fast query returns false for them.
    pub fn is_type_clone(&self, ty: Type) -> bool {
        if self.is_type_linear(ty) {
            return false;
        }
        if self.is_type_copy(ty) {
            return true;
        }
        if let TypeKind::Struct(struct_id) = ty.kind() {
            let struct_def = self.type_pool.struct_def(struct_id);
            if struct_def.is_clone {
                return true;
            }
        }
        false
    }

    /// Check that a preview feature is enabled.
    ///
    /// This is used to gate experimental features behind the `--preview` flag.
    /// Returns an error with a helpful message if the feature is not enabled.
    pub fn require_preview(
        &self,
        feature: gruel_util::PreviewFeature,
        what: &str,
        span: gruel_util::Span,
    ) -> gruel_util::CompileResult<()> {
        if self.preview_features.contains(&feature) {
            Ok(())
        } else {
            Err(gruel_util::CompileError::new(
                gruel_util::ErrorKind::PreviewFeatureRequired {
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

    // ========================================================================
    // Module-qualified type resolution
    // ========================================================================

    /// Resolve a struct type through a module reference.
    ///
    /// Used for qualified struct literals like `module.StructName { ... }`.
    /// The `module_ref` is an InstRef pointing to the result of an @import.
    /// Checks visibility: private structs are only accessible from the same directory.
    pub fn resolve_struct_through_module(
        &self,
        _module_ref: gruel_rir::InstRef,
        type_name: lasso::Spur,
        span: gruel_util::Span,
    ) -> gruel_util::CompileResult<StructId> {
        use gruel_util::{CompileError, ErrorKind};

        // Get the module type from the inst - we need to look up the AIR result
        // For now, use a simplified approach: look up the type name in the global scope
        // but require it to be exported from the module.
        //
        // A full implementation would:
        // 1. Resolve module_ref to get the ModuleId
        // 2. Look up the struct in that module's exports
        //
        // For now, we just look it up globally (works for single module imports)
        let type_name_str = self.interner.resolve(&type_name);

        // Try to find the struct globally
        let struct_id = self.get_struct(type_name).ok_or_else(|| {
            CompileError::new(ErrorKind::UnknownType(type_name_str.to_string()), span)
        })?;

        // Check visibility
        let struct_def = self.get_struct_def(struct_id);
        let accessing_file_id = span.file_id;
        let target_file_id = struct_def.file_id;

        if !self.is_accessible(accessing_file_id, target_file_id, struct_def.is_pub) {
            return Err(CompileError::new(
                ErrorKind::PrivateMemberAccess {
                    item_kind: "struct".to_string(),
                    name: type_name_str.to_string(),
                },
                span,
            ));
        }

        Ok(struct_id)
    }

    /// Resolve an enum type through a module reference.
    ///
    /// Used for qualified enum paths like `module.EnumName::Variant`.
    /// The `module_ref` is an InstRef pointing to the result of an @import.
    /// Checks visibility: private enums are only accessible from the same directory.
    pub fn resolve_enum_through_module(
        &self,
        _module_ref: gruel_rir::InstRef,
        type_name: lasso::Spur,
        span: gruel_util::Span,
    ) -> gruel_util::CompileResult<EnumId> {
        use gruel_util::{CompileError, ErrorKind};

        let type_name_str = self.interner.resolve(&type_name);

        // Try to find the enum globally
        let enum_id = self.get_enum(type_name).ok_or_else(|| {
            CompileError::new(ErrorKind::UnknownEnumType(type_name_str.to_string()), span)
        })?;

        // Check visibility
        let enum_def = self.get_enum_def(enum_id);
        let accessing_file_id = span.file_id;
        let target_file_id = enum_def.file_id;

        if !self.is_accessible(accessing_file_id, target_file_id, enum_def.is_pub) {
            return Err(CompileError::new(
                ErrorKind::PrivateMemberAccess {
                    item_kind: "enum".to_string(),
                    name: type_name_str.to_string(),
                },
                span,
            ));
        }

        Ok(enum_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time assertion that SemaContext is Send + Sync.
    /// This is critical for parallel function body analysis.
    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn test_sema_context_is_send_sync() {
        assert_send_sync::<SemaContext<'_>>();
    }
}

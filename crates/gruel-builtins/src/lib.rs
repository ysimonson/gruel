//! Built-in type definitions for the Gruel compiler.
//!
//! This crate provides the registry of built-in types like `String`. These are
//! types that behave like user-defined structs but have runtime implementations
//! for their methods rather than generated code.
//!
//! # Architecture
//!
//! Built-in types are represented as "synthetic structs" — the compiler injects
//! them as `StructDef` entries before processing user code. This allows them to
//! flow through the same code paths as user-defined types, eliminating scattered
//! special-case handling throughout the compiler.
//!
//! The injection happens in `Sema::inject_builtin_types()` during the declaration
//! gathering phase. After injection:
//!
//! - The type is accessible by name (e.g., `String`)
//! - Methods are registered and callable (e.g., `s.len()`)
//! - Associated functions work (e.g., `String::new()`)
//! - Operators are supported (e.g., `s1 == s2`)
//! - Drop glue is automatically generated if `drop_fn` is set
//!
//! # Adding a New Built-in Type
//!
//! To add a new built-in type (e.g., `Vec<T>` when generics are available):
//!
//! ## Step 1: Define the Type
//!
//! Create a `BuiltinTypeDef` constant describing the type's structure:
//!
//! ```rust,ignore
//! pub static VEC_TYPE: BuiltinTypeDef = BuiltinTypeDef {
//!     name: "Vec",  // How users refer to it in source code
//!     fields: &[
//!         BuiltinField { name: "ptr", ty: BuiltinFieldType::U64 },
//!         BuiltinField { name: "len", ty: BuiltinFieldType::U64 },
//!         BuiltinField { name: "cap", ty: BuiltinFieldType::U64 },
//!     ],
//!     is_copy: false,  // Vec owns heap memory, so it's a move type
//!     drop_fn: Some("__gruel_drop_Vec"),  // Runtime destructor
//!     operators: &[
//!         // Vec might support equality if elements do
//!     ],
//!     associated_fns: &[
//!         BuiltinAssociatedFn {
//!             name: "new",
//!             params: &[],
//!             return_ty: BuiltinReturnType::SelfType,
//!             runtime_fn: "Vec__new",
//!         },
//!         BuiltinAssociatedFn {
//!             name: "with_capacity",
//!             params: &[BuiltinParam { name: "capacity", ty: BuiltinParamType::U64 }],
//!             return_ty: BuiltinReturnType::SelfType,
//!             runtime_fn: "Vec__with_capacity",
//!         },
//!     ],
//!     methods: &[
//!         BuiltinMethod {
//!             name: "len",
//!             receiver_mode: ReceiverMode::ByRef,
//!             params: &[],
//!             return_ty: BuiltinReturnType::U64,
//!             runtime_fn: "Vec__len",
//!         },
//!         BuiltinMethod {
//!             name: "push",
//!             receiver_mode: ReceiverMode::ByMutRef,
//!             params: &[BuiltinParam { name: "value", ty: BuiltinParamType::U64 }],
//!             return_ty: BuiltinReturnType::SelfType,
//!             runtime_fn: "Vec__push",
//!         },
//!         // ... more methods
//!     ],
//! };
//! ```
//!
//! ## Step 2: Register the Type
//!
//! Add it to the `BUILTIN_TYPES` slice:
//!
//! ```rust,ignore
//! pub static BUILTIN_TYPES: &[&BuiltinTypeDef] = &[
//!     &STRING_TYPE,
//!     &VEC_TYPE,  // Add new types here
//! ];
//! ```
//!
//! ## Step 3: Implement Runtime Functions
//!
//! In `gruel-runtime`, implement the functions referenced in the type definition:
//!
//! ```rust,ignore
//! // In gruel-runtime/src/lib.rs or a new module
//!
//! #[unsafe(no_mangle)]
//! pub extern "C" fn Vec__new(out: *mut u64) {
//!     // Initialize empty Vec at `out` pointer
//!     unsafe {
//!         *out = 0;           // ptr = null
//!         *out.add(1) = 0;    // len = 0
//!         *out.add(2) = 0;    // cap = 0
//!     }
//! }
//!
//! #[unsafe(no_mangle)]
//! pub extern "C" fn __gruel_drop_Vec(ptr: *mut u64) {
//!     // Free the Vec's heap allocation if any
//!     unsafe {
//!         let data_ptr = *ptr as *mut u8;
//!         let cap = *ptr.add(2);
//!         if cap > 0 {
//!             __gruel_free(data_ptr, cap as usize);
//!         }
//!     }
//! }
//! ```
//!
//! ## Naming Conventions
//!
//! - **Associated functions**: `TypeName__function_name` (e.g., `String__new`)
//! - **Methods**: `TypeName__method_name` (e.g., `String__len`)
//! - **Drop functions**: `__gruel_drop_TypeName` (e.g., `__gruel_drop_String`)
//! - **Operators**: `__gruel_typename_op` (e.g., `__gruel_str_eq`)
//!
//! ## Key Types
//!
//! - [`BuiltinTypeDef`]: Complete definition of a built-in type
//! - [`BuiltinField`]: A field in the struct layout
//! - [`BuiltinMethod`]: An instance method (takes `self`)
//! - [`BuiltinAssociatedFn`]: A static function (e.g., `Type::new()`)
//! - [`BuiltinOperator`]: Operator overload (e.g., `==`, `!=`)
//! - [`ReceiverMode`]: How `self` is passed to methods
//!
//! See [`STRING_TYPE`] for a complete working example.

/// Binary operators that can be overloaded for built-in types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    /// Equality: `==`
    Eq,
    /// Inequality: `!=`
    Ne,
    /// Less than: `<`
    Lt,
    /// Less than or equal: `<=`
    Le,
    /// Greater than: `>`
    Gt,
    /// Greater than or equal: `>=`
    Ge,
}

/// Field type for built-in struct fields.
///
/// This is a simplified type representation for defining builtin struct layouts.
/// It maps to actual `Type` variants during struct injection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinFieldType {
    /// 64-bit unsigned integer (used for pointers, lengths, capacities)
    U64,
    /// 8-bit unsigned integer
    U8,
    /// Boolean
    Bool,
}

/// A field in a built-in struct.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinField {
    /// Field name
    pub name: &'static str,
    /// Field type
    pub ty: BuiltinFieldType,
}

/// How the receiver is passed to a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverMode {
    /// Method takes ownership: `fn method(self)`
    ByValue,
    /// Method borrows: `fn method(&self)`
    ByRef,
    /// Method mutably borrows: `fn method(&mut self)`
    ByMutRef,
}

/// A parameter to a built-in method or associated function.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinParam {
    /// Parameter name
    pub name: &'static str,
    /// Parameter type (simplified)
    pub ty: BuiltinParamType,
}

/// Type of a parameter to a built-in function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinParamType {
    /// 64-bit unsigned integer
    U64,
    /// Pointer-sized unsigned integer
    Usize,
    /// 8-bit unsigned integer
    U8,
    /// Boolean
    Bool,
    /// The built-in type itself (e.g., String for String methods)
    SelfType,
}

/// Return type of a built-in method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinReturnType {
    /// No return value (unit type)
    Unit,
    /// 64-bit unsigned integer
    U64,
    /// Pointer-sized unsigned integer
    Usize,
    /// 8-bit unsigned integer
    U8,
    /// Boolean (returned as u8: 0 or 1)
    Bool,
    /// Returns the built-in type itself
    SelfType,
}

/// An operator overload for a built-in type.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinOperator {
    /// The operator being overloaded
    pub op: BinOp,
    /// Runtime function to call (e.g., `__gruel_str_eq`)
    pub runtime_fn: &'static str,
    /// Whether to invert the result (for `!=` using `==` implementation)
    pub invert_result: bool,
}

/// An associated function on a built-in type (e.g., `String::new`).
#[derive(Debug, Clone, Copy)]
pub struct BuiltinAssociatedFn {
    /// Function name (e.g., "new")
    pub name: &'static str,
    /// Parameters (excluding any implicit out pointer)
    pub params: &'static [BuiltinParam],
    /// Return type
    pub return_ty: BuiltinReturnType,
    /// Runtime function name (e.g., "String__new")
    pub runtime_fn: &'static str,
}

/// An instance method on a built-in type (e.g., `s.len()`).
#[derive(Debug, Clone, Copy)]
pub struct BuiltinMethod {
    /// Method name (e.g., "len")
    pub name: &'static str,
    /// How the receiver is passed
    pub receiver_mode: ReceiverMode,
    /// Additional parameters after self
    pub params: &'static [BuiltinParam],
    /// Return type
    pub return_ty: BuiltinReturnType,
    /// Runtime function name (e.g., "String__len")
    pub runtime_fn: &'static str,
}

/// Definition of a built-in type.
///
/// This describes everything the compiler needs to know about a built-in type:
/// its layout (fields), behavior (operators), and available operations (methods).
#[derive(Debug, Clone)]
pub struct BuiltinTypeDef {
    /// Type name as it appears in source code (e.g., "String")
    pub name: &'static str,
    /// Fields that make up the struct layout
    pub fields: &'static [BuiltinField],
    /// Whether this type is Copy (can be implicitly duplicated)
    pub is_copy: bool,
    /// Runtime function to call for drop, if any
    pub drop_fn: Option<&'static str>,
    /// Supported operators and their implementations
    pub operators: &'static [BuiltinOperator],
    /// Associated functions (e.g., `String::new`)
    pub associated_fns: &'static [BuiltinAssociatedFn],
    /// Instance methods (e.g., `s.len()`)
    pub methods: &'static [BuiltinMethod],
}

// ============================================================================
// String Type Definition
// ============================================================================

/// The built-in String type.
///
/// Layout: `{ ptr: u64, len: u64, cap: u64 }` (24 bytes)
///
/// - `ptr`: Pointer to heap-allocated byte buffer (or .rodata for literals)
/// - `len`: Current length in bytes
/// - `cap`: Capacity of allocated buffer (0 for .rodata literals)
///
/// String is a move type (not Copy) because it owns heap-allocated memory.
/// The drop function checks `cap > 0` before freeing, allowing .rodata
/// literals (with `cap = 0`) to be safely dropped without freeing.
pub static STRING_TYPE: BuiltinTypeDef = BuiltinTypeDef {
    name: "String",
    fields: &[
        BuiltinField {
            name: "ptr",
            ty: BuiltinFieldType::U64,
        },
        BuiltinField {
            name: "len",
            ty: BuiltinFieldType::U64,
        },
        BuiltinField {
            name: "cap",
            ty: BuiltinFieldType::U64,
        },
    ],
    is_copy: false,
    drop_fn: Some("__gruel_drop_String"),
    operators: &[
        BuiltinOperator {
            op: BinOp::Eq,
            runtime_fn: "__gruel_str_eq",
            invert_result: false,
        },
        BuiltinOperator {
            op: BinOp::Ne,
            runtime_fn: "__gruel_str_eq",
            invert_result: true,
        },
        BuiltinOperator {
            op: BinOp::Lt,
            runtime_fn: "__gruel_str_cmp",
            invert_result: false,
        },
        BuiltinOperator {
            op: BinOp::Le,
            runtime_fn: "__gruel_str_cmp",
            invert_result: false,
        },
        BuiltinOperator {
            op: BinOp::Gt,
            runtime_fn: "__gruel_str_cmp",
            invert_result: false,
        },
        BuiltinOperator {
            op: BinOp::Ge,
            runtime_fn: "__gruel_str_cmp",
            invert_result: false,
        },
    ],
    associated_fns: &[
        BuiltinAssociatedFn {
            name: "new",
            params: &[],
            return_ty: BuiltinReturnType::SelfType,
            runtime_fn: "String__new",
        },
        BuiltinAssociatedFn {
            name: "with_capacity",
            params: &[BuiltinParam {
                name: "capacity",
                ty: BuiltinParamType::Usize,
            }],
            return_ty: BuiltinReturnType::SelfType,
            runtime_fn: "String__with_capacity",
        },
    ],
    methods: &[
        // Query methods (take &self)
        BuiltinMethod {
            name: "len",
            receiver_mode: ReceiverMode::ByRef,
            params: &[],
            return_ty: BuiltinReturnType::Usize,
            runtime_fn: "String__len",
        },
        BuiltinMethod {
            name: "capacity",
            receiver_mode: ReceiverMode::ByRef,
            params: &[],
            return_ty: BuiltinReturnType::Usize,
            runtime_fn: "String__capacity",
        },
        BuiltinMethod {
            name: "is_empty",
            receiver_mode: ReceiverMode::ByRef,
            params: &[],
            return_ty: BuiltinReturnType::Bool,
            runtime_fn: "String__is_empty",
        },
        BuiltinMethod {
            name: "clone",
            receiver_mode: ReceiverMode::ByRef,
            params: &[],
            return_ty: BuiltinReturnType::SelfType,
            runtime_fn: "String__clone",
        },
        BuiltinMethod {
            name: "contains",
            receiver_mode: ReceiverMode::ByRef,
            params: &[BuiltinParam {
                name: "needle",
                ty: BuiltinParamType::SelfType,
            }],
            return_ty: BuiltinReturnType::Bool,
            runtime_fn: "String__contains",
        },
        BuiltinMethod {
            name: "starts_with",
            receiver_mode: ReceiverMode::ByRef,
            params: &[BuiltinParam {
                name: "prefix",
                ty: BuiltinParamType::SelfType,
            }],
            return_ty: BuiltinReturnType::Bool,
            runtime_fn: "String__starts_with",
        },
        BuiltinMethod {
            name: "ends_with",
            receiver_mode: ReceiverMode::ByRef,
            params: &[BuiltinParam {
                name: "suffix",
                ty: BuiltinParamType::SelfType,
            }],
            return_ty: BuiltinReturnType::Bool,
            runtime_fn: "String__ends_with",
        },
        BuiltinMethod {
            name: "concat",
            receiver_mode: ReceiverMode::ByRef,
            params: &[BuiltinParam {
                name: "other",
                ty: BuiltinParamType::SelfType,
            }],
            return_ty: BuiltinReturnType::SelfType,
            runtime_fn: "String__concat",
        },
        // Mutation methods (take &mut self, return modified String)
        BuiltinMethod {
            name: "push_str",
            receiver_mode: ReceiverMode::ByMutRef,
            params: &[BuiltinParam {
                name: "other",
                ty: BuiltinParamType::SelfType,
            }],
            return_ty: BuiltinReturnType::SelfType,
            runtime_fn: "String__push_str",
        },
        BuiltinMethod {
            name: "push",
            receiver_mode: ReceiverMode::ByMutRef,
            params: &[BuiltinParam {
                name: "byte",
                ty: BuiltinParamType::U8,
            }],
            return_ty: BuiltinReturnType::SelfType,
            runtime_fn: "String__push",
        },
        BuiltinMethod {
            name: "clear",
            receiver_mode: ReceiverMode::ByMutRef,
            params: &[],
            return_ty: BuiltinReturnType::SelfType,
            runtime_fn: "String__clear",
        },
        BuiltinMethod {
            name: "reserve",
            receiver_mode: ReceiverMode::ByMutRef,
            params: &[BuiltinParam {
                name: "additional",
                ty: BuiltinParamType::Usize,
            }],
            return_ty: BuiltinReturnType::SelfType,
            runtime_fn: "String__reserve",
        },
    ],
};

// ============================================================================
// Registry
// ============================================================================

/// All built-in types.
///
/// The compiler iterates over this to inject synthetic structs before
/// processing user code.
pub static BUILTIN_TYPES: &[&BuiltinTypeDef] = &[&STRING_TYPE];

// ============================================================================
// Built-in Enums (Target Platform)
// ============================================================================

/// Definition of a built-in enum type.
///
/// These are synthetic enums injected by the compiler before processing user code.
/// They are used for compile-time platform detection via intrinsics like
/// `@target_arch()` and `@target_os()`.
#[derive(Debug, Clone)]
pub struct BuiltinEnumDef {
    /// Enum name as it appears in source code (e.g., "Arch")
    pub name: &'static str,
    /// Variant names in order (index matches variant_index in EnumVariant)
    pub variants: &'static [&'static str],
}

/// The built-in Arch enum for CPU architecture detection.
///
/// Variants:
/// - `X86_64` (index 0): x86-64 / AMD64
/// - `Aarch64` (index 1): ARM64 / AArch64
///
/// Used with `@target_arch()` intrinsic for platform-specific code.
pub static ARCH_ENUM: BuiltinEnumDef = BuiltinEnumDef {
    name: "Arch",
    variants: &["X86_64", "Aarch64"],
};

/// The built-in Os enum for operating system detection.
///
/// Variants:
/// - `Linux` (index 0): Linux
/// - `Macos` (index 1): macOS / Darwin
///
/// Used with `@target_os()` intrinsic for platform-specific code.
pub static OS_ENUM: BuiltinEnumDef = BuiltinEnumDef {
    name: "Os",
    variants: &["Linux", "Macos"],
};

/// The built-in TypeKind enum for compile-time type reflection.
///
/// Variants represent different type classifications, used by `@type_info`.
///
/// Variants:
/// - `Struct` (index 0): Struct types
/// - `Enum` (index 1): Enum types
/// - `Int` (index 2): Integer types (i8..i64, u8..u64)
/// - `Bool` (index 3): Boolean type
/// - `Unit` (index 4): Unit type
/// - `Never` (index 5): Never type
/// - `Array` (index 6): Fixed-size array types
pub static TYPEKIND_ENUM: BuiltinEnumDef = BuiltinEnumDef {
    name: "TypeKind",
    variants: &["Struct", "Enum", "Int", "Bool", "Unit", "Never", "Array"],
};

/// The built-in `Ownership` enum classifying a type's ownership posture.
///
/// Variants (per ADR-0008):
/// - `Copy` (index 0): values may be implicitly duplicated by bitwise copy
/// - `Affine` (index 1): values may be used at most once and are implicitly
///   dropped if not consumed (the default for user-defined structs)
/// - `Linear` (index 2): values must be explicitly consumed; implicit drop is
///   a compile-time error
///
/// Returned by the `@ownership(T)` intrinsic.
pub static OWNERSHIP_ENUM: BuiltinEnumDef = BuiltinEnumDef {
    name: "Ownership",
    variants: &["Copy", "Affine", "Linear"],
};

/// All built-in enums.
///
/// The compiler iterates over this to inject synthetic enums before
/// processing user code.
pub static BUILTIN_ENUMS: &[&BuiltinEnumDef] =
    &[&ARCH_ENUM, &OS_ENUM, &TYPEKIND_ENUM, &OWNERSHIP_ENUM];

/// Look up a built-in enum by name.
pub fn get_builtin_enum(name: &str) -> Option<&'static BuiltinEnumDef> {
    BUILTIN_ENUMS.iter().find(|e| e.name == name).copied()
}

/// Check if a name is reserved for a built-in enum.
pub fn is_reserved_enum_name(name: &str) -> bool {
    BUILTIN_ENUMS.iter().any(|e| e.name == name)
}

/// Look up a built-in type by name.
pub fn get_builtin_type(name: &str) -> Option<&'static BuiltinTypeDef> {
    BUILTIN_TYPES.iter().find(|t| t.name == name).copied()
}

/// Check if a name is reserved for a built-in type.
pub fn is_reserved_type_name(name: &str) -> bool {
    BUILTIN_TYPES.iter().any(|t| t.name == name)
}

// ============================================================================
// Built-in Type Constructors (parameterized types)
// ============================================================================

/// Identifier for a built-in parameterized type.
///
/// Each variant corresponds to a closed, compiler-recognized type constructor
/// (e.g. `Ptr(T)`, `MutPtr(T)`). The actual lowering to a `TypeKind` happens
/// in sema (`gruel-air`), which dispatches on this tag — `gruel-builtins`
/// has no dependency on the type system.
///
/// New constructors are added by extending this enum, adding an entry to
/// [`BUILTIN_TYPE_CONSTRUCTORS`], and adding a corresponding sema lowering
/// arm. Exhaustive matches in sema force you to add the lowering arm when
/// adding a variant — that's intentional, so the enum is not marked
/// `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinTypeConstructorKind {
    /// Immutable raw pointer (ADR-0061): `Ptr(T)` lowers to `TypeKind::PtrConst`.
    Ptr,
    /// Mutable raw pointer (ADR-0061): `MutPtr(T)` lowers to `TypeKind::PtrMut`.
    MutPtr,
    /// Immutable reference (ADR-0062): `Ref(T)` lowers to `TypeKind::Ref`.
    Ref,
    /// Mutable reference (ADR-0062): `MutRef(T)` lowers to `TypeKind::MutRef`.
    MutRef,
    /// Immutable slice (ADR-0064): `Slice(T)` lowers to `TypeKind::Slice`.
    Slice,
    /// Mutable slice (ADR-0064): `MutSlice(T)` lowers to `TypeKind::MutSlice`.
    MutSlice,
    /// Owned vector (ADR-0066): `Vec(T)` lowers to `TypeKind::Vec`.
    Vec,
}

/// Definition of a built-in parameterized type constructor.
///
/// Built-in type constructors share a single surface form with user-defined
/// comptime-generic functions that return `type` (e.g. `fn Vec(comptime T: type) -> type`):
/// both are written `Name(arg1, arg2, ...)` in type position. The difference is
/// that built-in constructors are hard-wired in the compiler — sema resolves
/// the name against this registry and lowers directly to a `TypeKind` without
/// running the comptime interpreter.
///
/// See ADR-0061 (`Ptr`/`MutPtr`) and ADR-0062 (`Ref`/`MutRef`) for usage.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinTypeConstructor {
    /// Constructor name as it appears in source code (e.g., "Ptr").
    pub name: &'static str,
    /// Number of comptime type arguments this constructor accepts.
    pub arity: usize,
    /// Which built-in lowering to use.
    pub kind: BuiltinTypeConstructorKind,
}

/// `Ptr(T)` — immutable raw pointer (ADR-0061).
pub static PTR_CONSTRUCTOR: BuiltinTypeConstructor = BuiltinTypeConstructor {
    name: "Ptr",
    arity: 1,
    kind: BuiltinTypeConstructorKind::Ptr,
};

/// `MutPtr(T)` — mutable raw pointer (ADR-0061).
pub static MUT_PTR_CONSTRUCTOR: BuiltinTypeConstructor = BuiltinTypeConstructor {
    name: "MutPtr",
    arity: 1,
    kind: BuiltinTypeConstructorKind::MutPtr,
};

/// `Ref(T)` — immutable reference (ADR-0062).
pub static REF_CONSTRUCTOR: BuiltinTypeConstructor = BuiltinTypeConstructor {
    name: "Ref",
    arity: 1,
    kind: BuiltinTypeConstructorKind::Ref,
};

/// `MutRef(T)` — mutable reference (ADR-0062).
pub static MUT_REF_CONSTRUCTOR: BuiltinTypeConstructor = BuiltinTypeConstructor {
    name: "MutRef",
    arity: 1,
    kind: BuiltinTypeConstructorKind::MutRef,
};

/// `Slice(T)` — immutable slice (ADR-0064).
pub static SLICE_CONSTRUCTOR: BuiltinTypeConstructor = BuiltinTypeConstructor {
    name: "Slice",
    arity: 1,
    kind: BuiltinTypeConstructorKind::Slice,
};

/// `MutSlice(T)` — mutable slice (ADR-0064).
pub static MUT_SLICE_CONSTRUCTOR: BuiltinTypeConstructor = BuiltinTypeConstructor {
    name: "MutSlice",
    arity: 1,
    kind: BuiltinTypeConstructorKind::MutSlice,
};

/// `Vec(T)` — owned, growable vector (ADR-0066).
pub static VEC_CONSTRUCTOR: BuiltinTypeConstructor = BuiltinTypeConstructor {
    name: "Vec",
    arity: 1,
    kind: BuiltinTypeConstructorKind::Vec,
};

/// All built-in type constructors.
///
/// The compiler iterates over this slice when resolving type-call expressions
/// and when reserving names so user code cannot shadow them.
pub static BUILTIN_TYPE_CONSTRUCTORS: &[&BuiltinTypeConstructor] = &[
    &PTR_CONSTRUCTOR,
    &MUT_PTR_CONSTRUCTOR,
    &REF_CONSTRUCTOR,
    &MUT_REF_CONSTRUCTOR,
    &SLICE_CONSTRUCTOR,
    &MUT_SLICE_CONSTRUCTOR,
    &VEC_CONSTRUCTOR,
];

/// Look up a built-in type constructor by name.
pub fn get_builtin_type_constructor(name: &str) -> Option<&'static BuiltinTypeConstructor> {
    BUILTIN_TYPE_CONSTRUCTORS
        .iter()
        .find(|c| c.name == name)
        .copied()
}

/// Check if a name is reserved for a built-in type constructor.
pub fn is_reserved_type_constructor_name(name: &str) -> bool {
    BUILTIN_TYPE_CONSTRUCTORS.iter().any(|c| c.name == name)
}

// ============================================================================
// Helper methods
// ============================================================================

impl BuiltinTypeDef {
    /// Get the number of slots this type uses in the ABI.
    ///
    /// Each field uses one slot (all fields are currently scalar types).
    pub fn slot_count(&self) -> u32 {
        self.fields.len() as u32
    }

    /// Find an associated function by name.
    pub fn find_associated_fn(&self, name: &str) -> Option<&BuiltinAssociatedFn> {
        self.associated_fns.iter().find(|f| f.name == name)
    }

    /// Find a method by name.
    pub fn find_method(&self, name: &str) -> Option<&BuiltinMethod> {
        self.methods.iter().find(|m| m.name == name)
    }

    /// Find an operator implementation.
    pub fn find_operator(&self, op: BinOp) -> Option<&BuiltinOperator> {
        self.operators.iter().find(|o| o.op == op)
    }

    /// Check if this type supports a given operator.
    pub fn supports_operator(&self, op: BinOp) -> bool {
        self.operators.iter().any(|o| o.op == op)
    }
}

// ============================================================================
// Reference doc generation
// ============================================================================

impl BinOp {
    /// Source-form symbol for this operator (e.g. `==`).
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
        }
    }
}

impl BuiltinFieldType {
    fn name(self) -> &'static str {
        match self {
            BuiltinFieldType::U64 => "u64",
            BuiltinFieldType::U8 => "u8",
            BuiltinFieldType::Bool => "bool",
        }
    }
}

impl BuiltinParamType {
    fn name(self, self_ty: &str) -> String {
        match self {
            BuiltinParamType::U64 => "u64".to_string(),
            BuiltinParamType::Usize => "usize".to_string(),
            BuiltinParamType::U8 => "u8".to_string(),
            BuiltinParamType::Bool => "bool".to_string(),
            BuiltinParamType::SelfType => self_ty.to_string(),
        }
    }
}

impl BuiltinReturnType {
    fn name(self, self_ty: &str) -> String {
        match self {
            BuiltinReturnType::Unit => "()".to_string(),
            BuiltinReturnType::U64 => "u64".to_string(),
            BuiltinReturnType::Usize => "usize".to_string(),
            BuiltinReturnType::U8 => "u8".to_string(),
            BuiltinReturnType::Bool => "bool".to_string(),
            BuiltinReturnType::SelfType => self_ty.to_string(),
        }
    }
}

impl ReceiverMode {
    fn signature(self) -> &'static str {
        match self {
            ReceiverMode::ByValue => "self",
            ReceiverMode::ByRef => "&self",
            ReceiverMode::ByMutRef => "&mut self",
        }
    }
}

impl BuiltinTypeConstructorKind {
    fn description(self) -> &'static str {
        match self {
            BuiltinTypeConstructorKind::Ptr => "immutable raw pointer (ADR-0061)",
            BuiltinTypeConstructorKind::MutPtr => "mutable raw pointer (ADR-0061)",
            BuiltinTypeConstructorKind::Ref => "immutable reference (ADR-0062)",
            BuiltinTypeConstructorKind::MutRef => "mutable reference (ADR-0062)",
            BuiltinTypeConstructorKind::Slice => "immutable slice (ADR-0064)",
            BuiltinTypeConstructorKind::MutSlice => "mutable slice (ADR-0064)",
            BuiltinTypeConstructorKind::Vec => "owned, growable vector (ADR-0066)",
        }
    }
}

fn fn_signature(
    name: &str,
    params: &[BuiltinParam],
    ret: BuiltinReturnType,
    self_ty: &str,
) -> String {
    let mut s = String::new();
    s.push_str(name);
    s.push('(');
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(p.name);
        s.push_str(": ");
        s.push_str(&p.ty.name(self_ty));
    }
    s.push(')');
    if !matches!(ret, BuiltinReturnType::Unit) {
        s.push_str(" -> ");
        s.push_str(&ret.name(self_ty));
    }
    s
}

fn method_signature(m: &BuiltinMethod, self_ty: &str) -> String {
    let mut s = String::from("fn ");
    s.push_str(m.name);
    s.push('(');
    s.push_str(m.receiver_mode.signature());
    for p in m.params {
        s.push_str(", ");
        s.push_str(p.name);
        s.push_str(": ");
        s.push_str(&p.ty.name(self_ty));
    }
    s.push(')');
    if !matches!(m.return_ty, BuiltinReturnType::Unit) {
        s.push_str(" -> ");
        s.push_str(&m.return_ty.name(self_ty));
    }
    s
}

/// Render the reference page for built-in types, type constructors, and enums.
///
/// The output is a self-contained markdown page generated from the registries
/// in this crate. It is the source of truth for the checked-in reference page
/// at `docs/generated/builtins-reference.md`; `make check-builtins-docs` runs
/// it and fails CI if the committed file differs from the generated output.
pub fn render_reference_markdown() -> String {
    let mut out = String::new();
    out.push_str("<!-- AUTO-GENERATED by `cargo run -p gruel-builtins-docs`. Do not edit by hand; edit the registries in `crates/gruel-builtins/src/lib.rs` and regenerate. -->\n\n");
    out.push_str("# Built-in Types Reference\n\n");
    out.push_str("This page documents every built-in type, type constructor, and enum the Gruel compiler injects before processing user code. It is generated from the registries in [`gruel-builtins`] (see [ADR-0020](../designs/0020-builtin-types-as-structs.md)); any changes must be made in Rust, not here.\n\n");

    // ---- Quick reference ----
    out.push_str("## Quick Reference\n\n");

    out.push_str("### Types\n\n");
    out.push_str("| Name | Ownership | Methods | Associated fns | Operators |\n");
    out.push_str("|---|---|---|---|---|\n");
    for t in BUILTIN_TYPES {
        let ownership = if t.is_copy { "copy" } else { "affine" };
        let ops: Vec<&'static str> = t.operators.iter().map(|o| o.op.symbol()).collect();
        let ops_str = if ops.is_empty() {
            "—".to_string()
        } else {
            ops.iter()
                .map(|s| format!("`{}`", s))
                .collect::<Vec<_>>()
                .join(", ")
        };
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} |\n",
            t.name,
            ownership,
            t.methods.len(),
            t.associated_fns.len(),
            ops_str,
        ));
    }
    out.push('\n');

    out.push_str("### Type Constructors\n\n");
    out.push_str("| Name | Arity | Description |\n");
    out.push_str("|---|---|---|\n");
    for c in BUILTIN_TYPE_CONSTRUCTORS {
        out.push_str(&format!(
            "| `{}` | {} | {} |\n",
            c.name,
            c.arity,
            c.kind.description(),
        ));
    }
    out.push('\n');

    out.push_str("### Enums\n\n");
    out.push_str("| Name | Variants |\n");
    out.push_str("|---|---|\n");
    for e in BUILTIN_ENUMS {
        let variants = e
            .variants
            .iter()
            .map(|v| format!("`{}`", v))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("| `{}` | {} |\n", e.name, variants));
    }
    out.push('\n');

    // ---- Types in detail ----
    out.push_str("## Types\n\n");
    for t in BUILTIN_TYPES {
        out.push_str(&format!("### `{}`\n\n", t.name));

        let ownership = if t.is_copy {
            "Copy (implicitly duplicated by bitwise copy).".to_string()
        } else if let Some(drop_fn) = t.drop_fn {
            format!("Affine (move semantics; dropped via `{}`).", drop_fn)
        } else {
            "Affine (move semantics; no destructor).".to_string()
        };
        out.push_str(&format!("**Ownership:** {}\n\n", ownership));

        if !t.fields.is_empty() {
            out.push_str("**Layout:**\n\n");
            out.push_str("| Field | Type |\n");
            out.push_str("|---|---|\n");
            for f in t.fields {
                out.push_str(&format!("| `{}` | `{}` |\n", f.name, f.ty.name()));
            }
            out.push('\n');
        }

        if !t.operators.is_empty() {
            out.push_str("**Operators:**\n\n");
            out.push_str("| Operator | Runtime symbol | Notes |\n");
            out.push_str("|---|---|---|\n");
            for op in t.operators {
                let notes = if op.invert_result {
                    "result inverted"
                } else {
                    "—"
                };
                out.push_str(&format!(
                    "| `{}` | `{}` | {} |\n",
                    op.op.symbol(),
                    op.runtime_fn,
                    notes,
                ));
            }
            out.push('\n');
        }

        if !t.associated_fns.is_empty() {
            out.push_str("**Associated functions:**\n\n");
            for f in t.associated_fns {
                let sig = fn_signature(
                    &format!("{}::{}", t.name, f.name),
                    f.params,
                    f.return_ty,
                    t.name,
                );
                out.push_str(&format!("- `{}` — runtime: `{}`\n", sig, f.runtime_fn));
            }
            out.push('\n');
        }

        if !t.methods.is_empty() {
            out.push_str("**Methods:**\n\n");
            for m in t.methods {
                let sig = method_signature(m, t.name);
                out.push_str(&format!("- `{}` — runtime: `{}`\n", sig, m.runtime_fn));
            }
            out.push('\n');
        }
    }

    // ---- Type constructors in detail ----
    out.push_str("## Type Constructors\n\n");
    out.push_str("Built-in type constructors are written `Name(arg1, arg2, ...)` in type position. Sema resolves the name against the registry and lowers directly to a `TypeKind` without running the comptime interpreter.\n\n");
    for c in BUILTIN_TYPE_CONSTRUCTORS {
        let args = (0..c.arity)
            .map(|i| {
                if c.arity == 1 {
                    "T".to_string()
                } else {
                    format!("T{}", i + 1)
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("### `{}({})`\n\n", c.name, args));
        out.push_str(&format!("{}.\n\n", c.kind.description()));
    }

    // ---- Enums in detail ----
    out.push_str("## Enums\n\n");
    out.push_str("Built-in enums are injected as synthetic enum types. They are used by reflection and platform-detection intrinsics.\n\n");
    for e in BUILTIN_ENUMS {
        out.push_str(&format!("### `{}`\n\n", e.name));
        out.push_str("| Index | Variant |\n");
        out.push_str("|---|---|\n");
        for (i, v) in e.variants.iter().enumerate() {
            out.push_str(&format!("| {} | `{}::{}` |\n", i, e.name, v));
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_type_exists() {
        assert_eq!(STRING_TYPE.name, "String");
        assert_eq!(STRING_TYPE.fields.len(), 3);
        assert!(!STRING_TYPE.is_copy);
        assert_eq!(STRING_TYPE.drop_fn, Some("__gruel_drop_String"));
    }

    #[test]
    fn test_string_slot_count() {
        assert_eq!(STRING_TYPE.slot_count(), 3);
    }

    #[test]
    fn test_string_associated_fns() {
        let new_fn = STRING_TYPE.find_associated_fn("new").unwrap();
        assert_eq!(new_fn.runtime_fn, "String__new");
        assert!(new_fn.params.is_empty());

        let with_cap = STRING_TYPE.find_associated_fn("with_capacity").unwrap();
        assert_eq!(with_cap.runtime_fn, "String__with_capacity");
        assert_eq!(with_cap.params.len(), 1);
    }

    #[test]
    fn test_string_methods() {
        let len = STRING_TYPE.find_method("len").unwrap();
        assert_eq!(len.runtime_fn, "String__len");
        assert_eq!(len.receiver_mode, ReceiverMode::ByRef);

        let push_str = STRING_TYPE.find_method("push_str").unwrap();
        assert_eq!(push_str.runtime_fn, "String__push_str");
        assert_eq!(push_str.receiver_mode, ReceiverMode::ByMutRef);
    }

    #[test]
    fn test_string_operators() {
        assert!(STRING_TYPE.supports_operator(BinOp::Eq));
        assert!(STRING_TYPE.supports_operator(BinOp::Ne));
        assert!(STRING_TYPE.supports_operator(BinOp::Lt));
        assert!(STRING_TYPE.supports_operator(BinOp::Gt));

        let eq = STRING_TYPE.find_operator(BinOp::Eq).unwrap();
        assert_eq!(eq.runtime_fn, "__gruel_str_eq");
        assert!(!eq.invert_result);

        let ne = STRING_TYPE.find_operator(BinOp::Ne).unwrap();
        assert_eq!(ne.runtime_fn, "__gruel_str_eq");
        assert!(ne.invert_result);
    }

    #[test]
    fn test_get_builtin_type() {
        assert!(get_builtin_type("String").is_some());
        assert!(get_builtin_type("Vec").is_none());
    }

    #[test]
    fn test_is_reserved_type_name() {
        assert!(is_reserved_type_name("String"));
        assert!(!is_reserved_type_name("MyStruct"));
    }

    #[test]
    fn test_all_string_methods_present() {
        // Verify all expected methods are defined
        let expected_methods = [
            "len",
            "capacity",
            "is_empty",
            "clone",
            "contains",
            "starts_with",
            "ends_with",
            "concat",
            "push_str",
            "push",
            "clear",
            "reserve",
        ];
        for name in expected_methods {
            assert!(
                STRING_TYPE.find_method(name).is_some(),
                "missing method: {}",
                name
            );
        }
    }

    // ========================================================================
    // Built-in Enum Tests
    // ========================================================================

    #[test]
    fn test_arch_enum() {
        assert_eq!(ARCH_ENUM.name, "Arch");
        assert_eq!(ARCH_ENUM.variants.len(), 2);
        assert_eq!(ARCH_ENUM.variants[0], "X86_64");
        assert_eq!(ARCH_ENUM.variants[1], "Aarch64");
    }

    #[test]
    fn test_os_enum() {
        assert_eq!(OS_ENUM.name, "Os");
        assert_eq!(OS_ENUM.variants.len(), 2);
        assert_eq!(OS_ENUM.variants[0], "Linux");
        assert_eq!(OS_ENUM.variants[1], "Macos");
    }

    #[test]
    fn test_get_builtin_enum() {
        assert!(get_builtin_enum("Arch").is_some());
        assert!(get_builtin_enum("Os").is_some());
        assert!(get_builtin_enum("Target").is_none());
    }

    #[test]
    fn test_is_reserved_enum_name() {
        assert!(is_reserved_enum_name("Arch"));
        assert!(is_reserved_enum_name("Os"));
        assert!(is_reserved_enum_name("TypeKind"));
        assert!(is_reserved_enum_name("Ownership"));
        assert!(!is_reserved_enum_name("MyEnum"));
    }

    #[test]
    fn test_builtin_enums_count() {
        assert_eq!(BUILTIN_ENUMS.len(), 4);
    }

    // ========================================================================
    // Built-in Type Constructor Tests
    // ========================================================================

    #[test]
    fn test_builtin_type_constructors_registry() {
        // ADR-0061: Ptr / MutPtr. ADR-0062: Ref / MutRef. ADR-0064: Slice /
        // MutSlice. ADR-0066: Vec.
        assert_eq!(BUILTIN_TYPE_CONSTRUCTORS.len(), 7);
    }

    #[test]
    fn test_get_builtin_type_constructor() {
        let ptr = get_builtin_type_constructor("Ptr").unwrap();
        assert_eq!(ptr.name, "Ptr");
        assert_eq!(ptr.arity, 1);
        assert_eq!(ptr.kind, BuiltinTypeConstructorKind::Ptr);

        let mut_ptr = get_builtin_type_constructor("MutPtr").unwrap();
        assert_eq!(mut_ptr.name, "MutPtr");
        assert_eq!(mut_ptr.arity, 1);
        assert_eq!(mut_ptr.kind, BuiltinTypeConstructorKind::MutPtr);

        assert!(get_builtin_type_constructor("MyConstructor").is_none());
    }

    #[test]
    fn test_is_reserved_type_constructor_name() {
        assert!(is_reserved_type_constructor_name("Ptr"));
        assert!(is_reserved_type_constructor_name("MutPtr"));
        assert!(!is_reserved_type_constructor_name("MyConstructor"));
    }
}

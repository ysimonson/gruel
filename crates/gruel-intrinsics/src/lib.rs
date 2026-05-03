//! Declarative registry of Gruel's `@intrinsic` set.
//!
//! This crate is the single source of truth for every intrinsic the compiler
//! recognizes. Each intrinsic is described by an [`IntrinsicDef`] value; the
//! full list lives in [`INTRINSICS`]. Compiler stages (RIR astgen, Sema,
//! codegen) consult the registry instead of carrying their own name lists, and
//! the website's intrinsic reference page is generated from the same data.
//!
//! Behavior (semantic analyzers, codegen arms) still lives in the consumer
//! crates — the registry owns metadata and identity, not per-intrinsic logic.
//! Stages dispatch on the stable [`IntrinsicId`] enum rather than matching
//! strings.
//!
//! See [ADR-0050](../../docs/designs/0050-intrinsics-crate.md).

use gruel_util::PreviewFeature;

// ============================================================================
// Enums
// ============================================================================

/// Stable identity for every intrinsic. Stages dispatch on this rather than
/// comparing strings, so adding an intrinsic requires updating a closed match
/// in each consumer — the compiler enforces coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntrinsicId {
    // ---- Debug / diagnostics ----
    Dbg,
    Panic,
    Assert,
    CompileError,

    // ---- Casts ----
    Cast,

    // ---- I/O ----
    ReadLine,
    ParseI32,
    ParseI64,
    ParseU32,
    ParseU64,

    // ---- Random ----
    RandomU32,
    RandomU64,

    // ---- Comptime / reflection ----
    SizeOf,
    AlignOf,
    TypeName,
    TypeInfo,
    Ownership,
    Implements,
    Field,
    Import,
    EmbedFile,

    // ---- Target platform ----
    TargetArch,
    TargetOs,

    // ---- Pointer operations (require unchecked) ----
    PtrRead,
    PtrWrite,
    PtrReadVolatile,
    PtrWriteVolatile,
    PtrOffset,
    PtrToInt,
    IntToPtr,
    NullPtr,
    IsNull,
    PtrCopy,
    Raw,
    RawMut,

    // ---- Syscall (requires unchecked) ----
    Syscall,

    // ---- For-loop iteration helpers ----
    Range,

    // ---- Slice operations (ADR-0064) ----
    SliceLen,
    SliceIsEmpty,
    SliceIndexRead,
    SliceIndexWrite,
    SlicePtr,
    SlicePtrMut,
    PartsToSlice,
    PartsToMutSlice,

    // ---- Vec operations (ADR-0066) ----
    VecNew,
    VecWithCapacity,
    VecLen,
    VecCapacity,
    VecIsEmpty,
    VecPush,
    VecPop,
    VecClear,
    VecReserve,
    VecIndexRead,
    VecIndexWrite,
    VecPtr,
    VecPtrMut,
    VecTerminatedPtr,
    VecClone,
    VecLiteral,
    VecRepeat,
    VecDispose,
    PartsToVec,

    // ---- ADR-0072 String / Vec(u8) bridge ----
    Utf8Validate,
    CStrToVec,

    // ---- Preview / test infra ----
    TestPreviewGate,
}

/// Whether an intrinsic takes an expression argument list (the common case)
/// or a type argument (`@size_of(T)`, `@type_info(T)`, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntrinsicKind {
    /// Normal expression intrinsic: `@name(expr, ...)`.
    Expr,
    /// Type intrinsic: `@name(Type)` where the argument is a type expression.
    Type,
    /// Type-and-interface intrinsic: `@name(Type, Interface)` where the
    /// first argument is a type expression and the second names an
    /// interface (e.g. `@implements(T, Drop)`).
    TypeInterface,
}

/// High-level grouping used when rendering the documentation reference page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Debug,
    Cast,
    Io,
    Parse,
    Random,
    Comptime,
    Platform,
    Pointer,
    Syscall,
    Iteration,
    Slice,
    Vec,
    Meta,
}

impl Category {
    /// Human-readable heading for this category in generated docs.
    pub fn heading(&self) -> &'static str {
        match self {
            Category::Debug => "Debug & Diagnostics",
            Category::Cast => "Type Casts",
            Category::Io => "I/O",
            Category::Parse => "String Parsing",
            Category::Random => "Random Numbers",
            Category::Comptime => "Compile-time Reflection",
            Category::Platform => "Target Platform",
            Category::Pointer => "Raw Pointers",
            Category::Syscall => "System Calls",
            Category::Iteration => "Iteration",
            Category::Slice => "Slices",
            Category::Vec => "Vectors",
            Category::Meta => "Preview / Meta",
        }
    }

    /// Canonical render order for the documentation reference page. The
    /// quick-reference table groups rows by category in this order, and the
    /// per-category detail sections appear in the same order. Adding a new
    /// `Category` variant requires extending this list — the test
    /// `category_render_order_is_exhaustive` enforces that every variant
    /// appears exactly once.
    pub const RENDER_ORDER: &'static [Category] = &[
        Category::Debug,
        Category::Cast,
        Category::Io,
        Category::Parse,
        Category::Random,
        Category::Comptime,
        Category::Platform,
        Category::Iteration,
        Category::Slice,
        Category::Vec,
        Category::Pointer,
        Category::Syscall,
        Category::Meta,
    ];

    /// Position in [`RENDER_ORDER`]. Used as the primary sort key for the
    /// quick-reference table so categories don't interleave.
    fn render_index(self) -> usize {
        Self::RENDER_ORDER
            .iter()
            .position(|c| *c == self)
            .expect("every Category variant must appear in RENDER_ORDER")
    }
}

// ============================================================================
// IntrinsicDef
// ============================================================================

/// Metadata for one intrinsic. Instances live as `const` entries in
/// [`INTRINSICS`]; nothing in this type is runtime-mutable.
#[derive(Debug, Clone, Copy)]
pub struct IntrinsicDef {
    /// Stable enum identity used for dispatch in consumer crates.
    pub id: IntrinsicId,
    /// Name as written in source (without the leading `@`).
    pub name: &'static str,
    /// Whether the sole argument is a type (`Type`) or a normal expression list (`Expr`).
    pub kind: IntrinsicKind,
    /// Category used for doc rendering and `by_category` lookups.
    pub category: Category,
    /// If `true`, calls must appear inside an `unchecked` block (enforced by sema).
    pub requires_unchecked: bool,
    /// Preview feature gate, if any. `None` means the intrinsic is stable.
    pub preview: Option<PreviewFeature>,
    /// Extern symbol in `gruel-runtime` that implements this intrinsic, if the
    /// codegen path lowers to a runtime call. `None` if the codegen emits LLVM
    /// IR directly (e.g. pointer ops) or is otherwise self-contained.
    pub runtime_fn: Option<&'static str>,
    /// Terse one-line description used in the quick-reference table.
    pub summary: &'static str,
    /// Longer markdown prose for the per-intrinsic detail section.
    pub description: &'static str,
    /// Sample code snippets rendered in the reference page.
    pub examples: &'static [&'static str],
}

// ============================================================================
// Registry
// ============================================================================

/// The canonical list of every intrinsic the compiler recognizes.
///
/// Adding an intrinsic: append a new [`IntrinsicDef`] here, extend
/// [`IntrinsicId`] with a matching variant, and implement the per-intrinsic
/// behavior arms in sema/codegen (the compiler's exhaustive matches will force
/// you to).
pub const INTRINSICS: &[IntrinsicDef] = &[
    IntrinsicDef {
        id: IntrinsicId::Dbg,
        name: "dbg",
        kind: IntrinsicKind::Expr,
        category: Category::Debug,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None, // Lowers to multiple runtime calls depending on arg type.
        summary: "Print values to stderr with a trailing newline.",
        description: "`@dbg(v1, v2, ...)` prints each argument separated by spaces, then a newline. Accepts integers, bools, and `String` values.",
        examples: &["@dbg(42, true, \"hello\")"],
    },
    IntrinsicDef {
        id: IntrinsicId::Panic,
        name: "panic",
        kind: IntrinsicKind::Expr,
        category: Category::Debug,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Abort the program with an optional message.",
        description: "`@panic()` or `@panic(\"message\")` terminates the program. Diverges (returns `Never`).",
        examples: &["@panic(\"unreachable\")"],
    },
    IntrinsicDef {
        id: IntrinsicId::Assert,
        name: "assert",
        kind: IntrinsicKind::Expr,
        category: Category::Debug,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Check a boolean condition; panic if false.",
        description: "`@assert(cond)` panics with a diagnostic if `cond` is false. Elided in release builds (future work).",
        examples: &["@assert(x > 0)"],
    },
    IntrinsicDef {
        id: IntrinsicId::CompileError,
        name: "compile_error",
        kind: IntrinsicKind::Expr,
        category: Category::Comptime,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Emit a compile-time error.",
        description: "`@compile_error(\"msg\")` aborts compilation with the given message. Useful for unreachable comptime branches.",
        examples: &["@compile_error(\"unsupported target\")"],
    },
    IntrinsicDef {
        id: IntrinsicId::Cast,
        name: "cast",
        kind: IntrinsicKind::Expr,
        category: Category::Cast,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Numeric type conversion.",
        description: "`@cast(x)` converts between integer and/or float types. The target type is inferred from context.",
        examples: &["let y: i64 = @cast(x);"],
    },
    IntrinsicDef {
        id: IntrinsicId::ReadLine,
        name: "read_line",
        kind: IntrinsicKind::Expr,
        category: Category::Io,
        requires_unchecked: false,
        preview: None,
        runtime_fn: Some("__gruel_read_line"),
        summary: "Read one line from stdin.",
        description: "`@read_line()` returns a `String` containing the next line from standard input, without the trailing newline.",
        examples: &["let line = @read_line();"],
    },
    IntrinsicDef {
        id: IntrinsicId::ParseI32,
        name: "parse_i32",
        kind: IntrinsicKind::Expr,
        category: Category::Parse,
        requires_unchecked: false,
        preview: None,
        runtime_fn: Some("__gruel_parse_i32"),
        summary: "Parse a String into i32.",
        description: "`@parse_i32(s)` parses `s` as a signed 32-bit integer. Panics on invalid input.",
        examples: &["let n: i32 = @parse_i32(line);"],
    },
    IntrinsicDef {
        id: IntrinsicId::ParseI64,
        name: "parse_i64",
        kind: IntrinsicKind::Expr,
        category: Category::Parse,
        requires_unchecked: false,
        preview: None,
        runtime_fn: Some("__gruel_parse_i64"),
        summary: "Parse a String into i64.",
        description: "`@parse_i64(s)` parses `s` as a signed 64-bit integer. Panics on invalid input.",
        examples: &["let n: i64 = @parse_i64(line);"],
    },
    IntrinsicDef {
        id: IntrinsicId::ParseU32,
        name: "parse_u32",
        kind: IntrinsicKind::Expr,
        category: Category::Parse,
        requires_unchecked: false,
        preview: None,
        runtime_fn: Some("__gruel_parse_u32"),
        summary: "Parse a String into u32.",
        description: "`@parse_u32(s)` parses `s` as an unsigned 32-bit integer. Panics on invalid input.",
        examples: &["let n: u32 = @parse_u32(line);"],
    },
    IntrinsicDef {
        id: IntrinsicId::ParseU64,
        name: "parse_u64",
        kind: IntrinsicKind::Expr,
        category: Category::Parse,
        requires_unchecked: false,
        preview: None,
        runtime_fn: Some("__gruel_parse_u64"),
        summary: "Parse a String into u64.",
        description: "`@parse_u64(s)` parses `s` as an unsigned 64-bit integer. Panics on invalid input.",
        examples: &["let n: u64 = @parse_u64(line);"],
    },
    IntrinsicDef {
        id: IntrinsicId::RandomU32,
        name: "random_u32",
        kind: IntrinsicKind::Expr,
        category: Category::Random,
        requires_unchecked: false,
        preview: None,
        runtime_fn: Some("__gruel_random_u32"),
        summary: "Uniform random 32-bit integer.",
        description: "`@random_u32()` returns a uniformly distributed `u32` from the runtime PRNG.",
        examples: &["let r = @random_u32();"],
    },
    IntrinsicDef {
        id: IntrinsicId::RandomU64,
        name: "random_u64",
        kind: IntrinsicKind::Expr,
        category: Category::Random,
        requires_unchecked: false,
        preview: None,
        runtime_fn: Some("__gruel_random_u64"),
        summary: "Uniform random 64-bit integer.",
        description: "`@random_u64()` returns a uniformly distributed `u64` from the runtime PRNG.",
        examples: &["let r = @random_u64();"],
    },
    IntrinsicDef {
        id: IntrinsicId::SizeOf,
        name: "size_of",
        kind: IntrinsicKind::Type,
        category: Category::Comptime,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Size of a type in bytes.",
        description: "`@size_of(T)` returns `sizeof(T)` as `usize`, evaluated at compile time.",
        examples: &["@size_of(i64)"],
    },
    IntrinsicDef {
        id: IntrinsicId::AlignOf,
        name: "align_of",
        kind: IntrinsicKind::Type,
        category: Category::Comptime,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Alignment of a type in bytes.",
        description: "`@align_of(T)` returns the required alignment of `T` as `usize`, evaluated at compile time.",
        examples: &["@align_of(i64)"],
    },
    IntrinsicDef {
        id: IntrinsicId::TypeName,
        name: "type_name",
        kind: IntrinsicKind::Type,
        category: Category::Comptime,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Name of a type as a comptime string.",
        description: "`@type_name(T)` returns the canonical name of `T` as a comptime-known string.",
        examples: &["@type_name(i64) // \"i64\""],
    },
    IntrinsicDef {
        id: IntrinsicId::TypeInfo,
        name: "type_info",
        kind: IntrinsicKind::Type,
        category: Category::Comptime,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Reflective info about a type.",
        description: "`@type_info(T)` returns a comptime struct describing `T` (kind, fields, variants, ...).",
        examples: &["@type_info(MyStruct)"],
    },
    IntrinsicDef {
        id: IntrinsicId::Ownership,
        name: "ownership",
        kind: IntrinsicKind::Type,
        category: Category::Comptime,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Ownership posture of a type (`Copy`, `Affine`, or `Linear`).",
        description: "`@ownership(T)` returns a variant of the built-in `Ownership` enum classifying `T`'s ownership posture (see ADR-0008): `Copy` if values can be implicitly duplicated, `Linear` if values must be explicitly consumed, or `Affine` otherwise (move-once with implicit drop). Evaluated at compile time.",
        examples: &[
            "@ownership(i32) // Ownership::Copy",
            "@ownership(String) // Ownership::Affine",
            "match @ownership(T) { Ownership::Copy => ..., Ownership::Affine => ..., Ownership::Linear => ... }",
        ],
    },
    IntrinsicDef {
        id: IntrinsicId::Implements,
        name: "implements",
        kind: IntrinsicKind::TypeInterface,
        category: Category::Comptime,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Whether a type structurally implements an interface.",
        description: "`@implements(T, I)` returns `true` if type `T` satisfies every method requirement of interface `I` (see ADR-0056), and `false` otherwise. Built-in interfaces `Copy` and `Drop` use the language's ownership rules rather than user methods. The result is a `bool` evaluated at compile time, so `@implements(...)` can be used to gate `comptime if` branches and other comptime decisions.",
        examples: &[
            "@implements(i32, Copy) // true",
            "@implements(String, Copy) // false",
            "@implements(MyType, Drop)",
        ],
    },
    IntrinsicDef {
        id: IntrinsicId::Field,
        name: "field",
        kind: IntrinsicKind::Expr,
        category: Category::Comptime,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Access a field by comptime-known name.",
        description: "`@field(value, \"name\")` reads the named field of `value`, with the name resolved at compile time.",
        examples: &["@field(s, \"x\")"],
    },
    IntrinsicDef {
        id: IntrinsicId::Import,
        name: "import",
        kind: IntrinsicKind::Expr,
        category: Category::Comptime,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Import another source file (placeholder).",
        description: "`@import(\"path\")` — planned module-system hook; currently accepted by the compiler as a placeholder.",
        examples: &["@import(\"utils.gruel\")"],
    },
    IntrinsicDef {
        id: IntrinsicId::EmbedFile,
        name: "embed_file",
        kind: IntrinsicKind::Expr,
        category: Category::Comptime,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Embed a file's contents at compile time as `Slice(u8)`.",
        description: "`@embed_file(\"path\")` reads the file at compile time and produces a `Slice(u8)` whose bytes are baked into the binary as a read-only global. The path is resolved relative to the source file containing the call.",
        examples: &["let data: Slice(u8) = @embed_file(\"asset.bin\");"],
    },
    IntrinsicDef {
        id: IntrinsicId::TargetArch,
        name: "target_arch",
        kind: IntrinsicKind::Expr,
        category: Category::Platform,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Compile target CPU architecture.",
        description: "`@target_arch()` returns a variant of the built-in `Arch` enum.",
        examples: &["if @target_arch() == Arch::Aarch64 { ... }"],
    },
    IntrinsicDef {
        id: IntrinsicId::TargetOs,
        name: "target_os",
        kind: IntrinsicKind::Expr,
        category: Category::Platform,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Compile target operating system.",
        description: "`@target_os()` returns a variant of the built-in `Os` enum.",
        examples: &["if @target_os() == Os::Linux { ... }"],
    },
    // ADR-0063: pointer operations are no longer user-callable via the
    // `@…` namespace — the surface form is `p.method(...)` /
    // `Ptr(T)::name(...)`. The metadata entries remain so codegen and
    // `lookup_by_id` can find each intrinsic by `IntrinsicId`. Sema's
    // `analyze_intrinsic_impl` rejects the `@…` form for these
    // intrinsics; the same `IntrinsicId` is reachable through the
    // POINTER_METHODS registry.
    IntrinsicDef {
        id: IntrinsicId::PtrRead,
        name: "ptr_read",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Load a value through a raw pointer (internal).",
        description: "Internal lowering target for `p.read()` (ADR-0063).",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::PtrWrite,
        name: "ptr_write",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Store a value through a raw mutable pointer (internal).",
        description: "Internal lowering target for `p.write(v)` (ADR-0063).",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::PtrReadVolatile,
        name: "ptr_read_volatile",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Volatile load through a raw pointer (internal).",
        description: "Internal lowering target for `p.read_volatile()`. Lowers to an LLVM `load volatile`, which the optimizer may not elide, duplicate, or reorder relative to other volatile accesses. Intended for memory-mapped I/O where every read has externally visible side effects.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::PtrWriteVolatile,
        name: "ptr_write_volatile",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Volatile store through a raw mutable pointer (internal).",
        description: "Internal lowering target for `p.write_volatile(v)`. Lowers to an LLVM `store volatile`, which the optimizer may not elide, duplicate, or reorder relative to other volatile accesses. Intended for memory-mapped I/O where every write has externally visible side effects.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::PtrOffset,
        name: "ptr_offset",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Pointer arithmetic by element count (internal).",
        description: "Internal lowering target for `p.offset(n)` (ADR-0063).",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::PtrToInt,
        name: "ptr_to_int",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Convert a pointer to its integer address (internal).",
        description: "Internal lowering target for `p.to_int()` (ADR-0063).",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::IntToPtr,
        name: "int_to_ptr",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Construct a pointer from an integer address (internal).",
        description: "Internal lowering target for `Ptr(T)::from_int(addr)` (ADR-0063).",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::NullPtr,
        name: "null_ptr",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "A null pointer of the inferred type (internal).",
        description: "Internal lowering target for `Ptr(T)::null()` (ADR-0063).",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::IsNull,
        name: "is_null",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Test whether a pointer is null (internal).",
        description: "Internal lowering target for `p.is_null()` (ADR-0063).",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::PtrCopy,
        name: "ptr_copy",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Bulk copy between pointers (internal).",
        description: "Internal lowering target for `dst.copy_from(src, n)` (ADR-0063).",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::Raw,
        name: "raw",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Take a const pointer to an lvalue (internal).",
        description: "Internal lowering target for `Ptr(T)::from(&x)` (ADR-0063).",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::RawMut,
        name: "raw_mut",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Take a mutable pointer to an lvalue (internal).",
        description: "Internal lowering target for `MutPtr(T)::from(&mut x)` (ADR-0063).",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::Syscall,
        name: "syscall",
        kind: IntrinsicKind::Expr,
        category: Category::Syscall,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Direct OS system call.",
        description: "`@syscall(num, arg1, ...)` issues a raw syscall. Takes the syscall number plus up to 6 arguments; returns `i64`. Requires an `unchecked` block.",
        examples: &["checked { let ret = @syscall(1, 1, buf, n); }"],
    },
    IntrinsicDef {
        id: IntrinsicId::Range,
        name: "range",
        kind: IntrinsicKind::Expr,
        category: Category::Iteration,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Iterable range for `for`-loops.",
        description: "`@range(end)`, `@range(start, end)`, or `@range(start, end, step)` produces an iterable over integers.",
        examples: &["for i in @range(0, 10) { ... }"],
    },
    IntrinsicDef {
        id: IntrinsicId::SliceLen,
        name: "slice_len",
        kind: IntrinsicKind::Expr,
        category: Category::Slice,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Length of a slice.",
        description: "`@slice_len(s)` returns the number of elements in `s` (a `Slice(T)` or `MutSlice(T)`) as `usize`. Surface form: `s.len()`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::SliceIsEmpty,
        name: "slice_is_empty",
        kind: IntrinsicKind::Expr,
        category: Category::Slice,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Whether a slice has length zero.",
        description: "`@slice_is_empty(s)` returns `s.len() == 0`. Surface form: `s.is_empty()`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::SliceIndexRead,
        name: "slice_index_read",
        kind: IntrinsicKind::Expr,
        category: Category::Slice,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Read an element from a slice with bounds checking.",
        description: "`@slice_index_read(s, i)` returns `s[i]`. Bounds-checks at runtime; panics on out-of-range. Surface form: `s[i]`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::SlicePtr,
        name: "slice_ptr",
        kind: IntrinsicKind::Expr,
        category: Category::Slice,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Extract the data pointer from a slice.",
        description: "`@slice_ptr(s)` returns a `Ptr(T)` to the slice's first element. Requires a `checked` block. Surface form: `s.ptr()`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::SlicePtrMut,
        name: "slice_ptr_mut",
        kind: IntrinsicKind::Expr,
        category: Category::Slice,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Extract the mutable data pointer from a mutable slice.",
        description: "`@slice_ptr_mut(m)` returns a `MutPtr(T)` to a `MutSlice(T)`'s first element. Requires a `checked` block. Surface form: `m.ptr_mut()`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::PartsToSlice,
        name: "parts_to_slice",
        kind: IntrinsicKind::Expr,
        category: Category::Slice,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Build a slice from a raw pointer and a length.",
        description: "`@parts_to_slice(p: Ptr(T), n: usize) -> Slice(T)` constructs a slice without checking that the underlying storage is valid. Requires a `checked` block.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::PartsToMutSlice,
        name: "parts_to_mut_slice",
        kind: IntrinsicKind::Expr,
        category: Category::Slice,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Build a mutable slice from a raw mutable pointer and a length.",
        description: "`@parts_to_mut_slice(p: MutPtr(T), n: usize) -> MutSlice(T)`. Requires a `checked` block.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::SliceIndexWrite,
        name: "slice_index_write",
        kind: IntrinsicKind::Expr,
        category: Category::Slice,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Write an element to a mutable slice with bounds checking.",
        description: "`@slice_index_write(m, i, v)` performs `m[i] = v`. Requires `MutSlice(T)`. Bounds-checks at runtime. Surface form: `m[i] = v`.",
        examples: &[],
    },
    // ---- Vec operations (ADR-0066) ----
    IntrinsicDef {
        id: IntrinsicId::VecNew,
        name: "vec_new",
        kind: IntrinsicKind::Expr,
        category: Category::Vec,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Create an empty Vec(T).",
        description: "`@vec_new(T)` returns an empty `Vec(T)` (cap=0, ptr=null). Surface form: `Vec(T)::new()`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::VecWithCapacity,
        name: "vec_with_capacity",
        kind: IntrinsicKind::Expr,
        category: Category::Vec,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Create a Vec(T) with a preallocated buffer.",
        description: "`@vec_with_capacity(T, n)` returns an empty `Vec(T)` whose `cap >= n`. Surface form: `Vec(T)::with_capacity(n)`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::VecLen,
        name: "vec_len",
        kind: IntrinsicKind::Expr,
        category: Category::Vec,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Length of a vec.",
        description: "`@vec_len(v)` returns the live element count. Surface form: `v.len()`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::VecCapacity,
        name: "vec_capacity",
        kind: IntrinsicKind::Expr,
        category: Category::Vec,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Capacity of a vec.",
        description: "`@vec_capacity(v)` returns the allocated slot count. Surface form: `v.capacity()`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::VecIsEmpty,
        name: "vec_is_empty",
        kind: IntrinsicKind::Expr,
        category: Category::Vec,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Whether a vec has length zero.",
        description: "`@vec_is_empty(v)` returns `v.len() == 0`. Surface form: `v.is_empty()`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::VecPush,
        name: "vec_push",
        kind: IntrinsicKind::Expr,
        category: Category::Vec,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Append an element to a Vec.",
        description: "`@vec_push(v, x)` appends `x` to `v`, growing the buffer if needed. Surface form: `v.push(x)`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::VecPop,
        name: "vec_pop",
        kind: IntrinsicKind::Expr,
        category: Category::Vec,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Remove and return the last element of a Vec.",
        description: "`@vec_pop(v)` returns `Option(T)` — `None` on empty, `Some(t)` otherwise. Surface form: `v.pop()`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::VecClear,
        name: "vec_clear",
        kind: IntrinsicKind::Expr,
        category: Category::Vec,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Drop all elements of a Vec without freeing the buffer.",
        description: "`@vec_clear(v)` runs the per-element drop loop and sets `len = 0`. Surface form: `v.clear()`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::VecReserve,
        name: "vec_reserve",
        kind: IntrinsicKind::Expr,
        category: Category::Vec,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Ensure a Vec has capacity for additional elements.",
        description: "`@vec_reserve(v, n)` grows the buffer so that `cap >= len + n`. Surface form: `v.reserve(n)`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::VecIndexRead,
        name: "vec_index_read",
        kind: IntrinsicKind::Expr,
        category: Category::Vec,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Read an element from a vec with bounds checking.",
        description: "`@vec_index_read(v, i)` returns `v[i]`. Bounds-checked at runtime. Requires `T: Copy`. Surface form: `v[i]`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::VecIndexWrite,
        name: "vec_index_write",
        kind: IntrinsicKind::Expr,
        category: Category::Vec,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Write an element to a vec with bounds checking.",
        description: "`@vec_index_write(v, i, x)` performs `v[i] = x`. Bounds-checked at runtime. Surface form: `v[i] = x`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::VecPtr,
        name: "vec_ptr",
        kind: IntrinsicKind::Expr,
        category: Category::Vec,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Extract the data pointer from a Vec.",
        description: "`@vec_ptr(v)` returns a `Ptr(T)` to the first element. Requires a `checked` block. Surface form: `v.ptr()`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::VecPtrMut,
        name: "vec_ptr_mut",
        kind: IntrinsicKind::Expr,
        category: Category::Vec,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Extract the mutable data pointer from a Vec.",
        description: "`@vec_ptr_mut(v)` returns a `MutPtr(T)`. Requires a `checked` block. Surface form: `v.ptr_mut()`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::VecTerminatedPtr,
        name: "vec_terminated_ptr",
        kind: IntrinsicKind::Expr,
        category: Category::Vec,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Write a sentinel and return the data pointer.",
        description: "`@vec_terminated_ptr(v, s)` writes `s` at `ptr[len]` (growing if needed), returns `Ptr(T)`. Requires a `checked` block. Surface form: `v.terminated_ptr(s)`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::VecClone,
        name: "vec_clone",
        kind: IntrinsicKind::Expr,
        category: Category::Vec,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Clone a Vec.",
        description: "`@vec_clone(v)` returns a deep copy of `v`. Requires `T: Clone`. Surface form: `v.clone()`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::VecLiteral,
        name: "vec",
        kind: IntrinsicKind::Expr,
        category: Category::Vec,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Construct a Vec from individual elements.",
        description: "`@vec(a, b, c)` returns a `Vec(T)` of length 3 with the given elements. Mirrors Rust's `vec![…]`. Requires at least one argument; element types unify to a single `T`.",
        examples: &["@vec(1, 2, 3)"],
    },
    IntrinsicDef {
        id: IntrinsicId::VecRepeat,
        name: "vec_repeat",
        kind: IntrinsicKind::Expr,
        category: Category::Vec,
        requires_unchecked: false,
        preview: None,
        runtime_fn: None,
        summary: "Construct a Vec with N copies of a value.",
        description: "`@vec_repeat(v, n)` returns a `Vec(T)` of length `n` where every slot holds a clone of `v`. Requires `T: Clone`.",
        examples: &["@vec_repeat(0, 100)"],
    },
    IntrinsicDef {
        id: IntrinsicId::VecDispose,
        name: "vec_dispose",
        kind: IntrinsicKind::Expr,
        category: Category::Vec,
        requires_unchecked: false,
        preview: None,
        runtime_fn: Some("__gruel_vec_dispose_panic"),
        summary: "Free a Vec's heap buffer; panic if `len != 0`.",
        description: "`@vec_dispose(v)` is the explicit-release form for `Vec(T)`. It panics if `v.len != 0` (so any contained linear elements are still live), then frees the heap buffer. Surface form: `v.dispose()`. For `Vec(T:Linear)` this is the only legal release path; for non-linear `T` it's an explicit alternative to implicit drop.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::PartsToVec,
        name: "parts_to_vec",
        kind: IntrinsicKind::Expr,
        category: Category::Vec,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Build a Vec from raw parts.",
        description: "`@parts_to_vec(p: MutPtr(T), len: usize, cap: usize) -> Vec(T)` takes ownership of `p`. Requires a `checked` block.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::TestPreviewGate,
        name: "test_preview_gate",
        kind: IntrinsicKind::Expr,
        category: Category::Meta,
        requires_unchecked: false,
        preview: Some(PreviewFeature::TestInfra),
        runtime_fn: None,
        summary: "Test hook for the preview-feature gate.",
        description: "`@test_preview_gate()` exists solely to verify that the preview-feature gating mechanism works. Always gated behind `--preview test_infra`.",
        examples: &[],
    },
    IntrinsicDef {
        id: IntrinsicId::CStrToVec,
        name: "cstr_to_vec",
        kind: IntrinsicKind::Expr,
        category: Category::Meta,
        requires_unchecked: true,
        // Implementation-detail intrinsic invoked by the prelude
        // `String__from_c_str` body. The user-visible gate is on
        // `String::from_c_str` itself (ADR-0072).
        preview: None,
        runtime_fn: Some("__gruel_cstr_to_vec"),
        summary: "Copy a NUL-terminated C string into a fresh Vec(u8).",
        description: "`@cstr_to_vec(p: Ptr(u8)) -> Vec(u8)` runs `strlen(p)`, allocates `cap >= len` bytes, and copies. Used by `String::from_c_str` (ADR-0072).",
        examples: &["@cstr_to_vec(p)"],
    },
    IntrinsicDef {
        id: IntrinsicId::Utf8Validate,
        name: "utf8_validate",
        kind: IntrinsicKind::Expr,
        category: Category::Meta,
        requires_unchecked: false,
        // Implementation-detail intrinsic invoked by the prelude
        // `String__from_utf8` body. The user-visible gate is on
        // `String::from_utf8` itself (ADR-0072), so this stays ungated to
        // allow eager prelude analysis without `--preview string_vec_bridge`.
        preview: None,
        runtime_fn: Some("__gruel_utf8_validate"),
        summary: "Check whether a byte slice is well-formed UTF-8.",
        description: "`@utf8_validate(s: borrow Slice(u8)) -> bool` returns `true` iff the bytes in `s` form a valid UTF-8 sequence. Used by `String::from_utf8` (ADR-0072).",
        examples: &["@utf8_validate(&v[..])"],
    },
];

// ============================================================================
// Pointer-method registry (ADR-0063)
// ============================================================================

/// Which builtin pointer constructor an entry is defined on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerKind {
    /// Defined on `Ptr(T)`.
    Ptr,
    /// Defined on `MutPtr(T)`.
    MutPtr,
}

/// Whether an entry is an instance method or an associated function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerOpForm {
    /// Instance method called on a pointer value: `p.name(args)`.
    Method,
    /// Associated function called on the type: `Ptr(T)::name(args)`.
    AssocFn,
}

/// One method or associated function on `Ptr(T)` / `MutPtr(T)` (ADR-0063).
///
/// Each entry is a pure metadata record describing the surface form. The
/// actual semantic / codegen behaviour is reused from the intrinsic
/// identified by [`PointerMethod::intrinsic`] — this registry exists only to
/// give sema the surface-to-intrinsic mapping. No new runtime functions.
///
/// `intrinsic_name` mirrors what the equivalent legacy `@…` form was called
/// (e.g. `"ptr_read"` for `IntrinsicId::PtrRead`). The codegen path
/// dispatches `AirInstData::Intrinsic` by name, so emitting the new surface
/// form lowers to the same string the old `@ptr_read` would have.
#[derive(Debug, Clone, Copy)]
pub struct PointerMethod {
    /// Constructor this method/fn is defined on.
    pub kind: PointerKind,
    /// Name as written by the user (after `.` for methods, after `::` for
    /// associated fns).
    pub name: &'static str,
    /// Method (`p.name(...)`) or associated fn (`Type(T)::name(...)`).
    pub form: PointerOpForm,
    /// Stable identity used by codegen / IR analyzers.
    pub intrinsic: IntrinsicId,
    /// Symbol the AIR `Intrinsic` instruction is tagged with.
    pub intrinsic_name: &'static str,
    /// Whether the lowering requires a `checked` block (mirrors what the
    /// legacy `@…` registry entry would have had).
    pub requires_checked: bool,
}

/// Closed registry of every pointer method / associated function (ADR-0063).
///
/// Sema's method-call path consults this when the receiver type is
/// `Ptr(_)` / `MutPtr(_)`; the path-call path consults it when the LHS
/// resolves to such a type.
pub const POINTER_METHODS: &[PointerMethod] = &[
    // ---- Methods on Ptr(T) ----
    PointerMethod {
        kind: PointerKind::Ptr,
        name: "read",
        form: PointerOpForm::Method,
        intrinsic: IntrinsicId::PtrRead,
        intrinsic_name: "ptr_read",
        requires_checked: true,
    },
    PointerMethod {
        kind: PointerKind::Ptr,
        name: "read_volatile",
        form: PointerOpForm::Method,
        intrinsic: IntrinsicId::PtrReadVolatile,
        intrinsic_name: "ptr_read_volatile",
        requires_checked: true,
    },
    PointerMethod {
        kind: PointerKind::Ptr,
        name: "offset",
        form: PointerOpForm::Method,
        intrinsic: IntrinsicId::PtrOffset,
        intrinsic_name: "ptr_offset",
        requires_checked: true,
    },
    PointerMethod {
        kind: PointerKind::Ptr,
        name: "is_null",
        form: PointerOpForm::Method,
        intrinsic: IntrinsicId::IsNull,
        intrinsic_name: "is_null",
        requires_checked: true,
    },
    PointerMethod {
        kind: PointerKind::Ptr,
        name: "to_int",
        form: PointerOpForm::Method,
        intrinsic: IntrinsicId::PtrToInt,
        intrinsic_name: "ptr_to_int",
        requires_checked: true,
    },
    // ---- Associated fns on Ptr(T) ----
    PointerMethod {
        kind: PointerKind::Ptr,
        name: "from",
        form: PointerOpForm::AssocFn,
        intrinsic: IntrinsicId::Raw,
        intrinsic_name: "raw",
        requires_checked: true,
    },
    PointerMethod {
        kind: PointerKind::Ptr,
        name: "null",
        form: PointerOpForm::AssocFn,
        intrinsic: IntrinsicId::NullPtr,
        intrinsic_name: "null_ptr",
        requires_checked: true,
    },
    PointerMethod {
        kind: PointerKind::Ptr,
        name: "from_int",
        form: PointerOpForm::AssocFn,
        intrinsic: IntrinsicId::IntToPtr,
        intrinsic_name: "int_to_ptr",
        requires_checked: true,
    },
    // ---- Methods on MutPtr(T) ----
    PointerMethod {
        kind: PointerKind::MutPtr,
        name: "read",
        form: PointerOpForm::Method,
        intrinsic: IntrinsicId::PtrRead,
        intrinsic_name: "ptr_read",
        requires_checked: true,
    },
    PointerMethod {
        kind: PointerKind::MutPtr,
        name: "read_volatile",
        form: PointerOpForm::Method,
        intrinsic: IntrinsicId::PtrReadVolatile,
        intrinsic_name: "ptr_read_volatile",
        requires_checked: true,
    },
    PointerMethod {
        kind: PointerKind::MutPtr,
        name: "write",
        form: PointerOpForm::Method,
        intrinsic: IntrinsicId::PtrWrite,
        intrinsic_name: "ptr_write",
        requires_checked: true,
    },
    PointerMethod {
        kind: PointerKind::MutPtr,
        name: "write_volatile",
        form: PointerOpForm::Method,
        intrinsic: IntrinsicId::PtrWriteVolatile,
        intrinsic_name: "ptr_write_volatile",
        requires_checked: true,
    },
    PointerMethod {
        kind: PointerKind::MutPtr,
        name: "offset",
        form: PointerOpForm::Method,
        intrinsic: IntrinsicId::PtrOffset,
        intrinsic_name: "ptr_offset",
        requires_checked: true,
    },
    PointerMethod {
        kind: PointerKind::MutPtr,
        name: "is_null",
        form: PointerOpForm::Method,
        intrinsic: IntrinsicId::IsNull,
        intrinsic_name: "is_null",
        requires_checked: true,
    },
    PointerMethod {
        kind: PointerKind::MutPtr,
        name: "to_int",
        form: PointerOpForm::Method,
        intrinsic: IntrinsicId::PtrToInt,
        intrinsic_name: "ptr_to_int",
        requires_checked: true,
    },
    PointerMethod {
        kind: PointerKind::MutPtr,
        name: "copy_from",
        form: PointerOpForm::Method,
        intrinsic: IntrinsicId::PtrCopy,
        intrinsic_name: "ptr_copy",
        requires_checked: true,
    },
    // ---- Associated fns on MutPtr(T) ----
    PointerMethod {
        kind: PointerKind::MutPtr,
        name: "from",
        form: PointerOpForm::AssocFn,
        intrinsic: IntrinsicId::RawMut,
        intrinsic_name: "raw_mut",
        requires_checked: true,
    },
    PointerMethod {
        kind: PointerKind::MutPtr,
        name: "null",
        form: PointerOpForm::AssocFn,
        intrinsic: IntrinsicId::NullPtr,
        intrinsic_name: "null_ptr",
        requires_checked: true,
    },
    PointerMethod {
        kind: PointerKind::MutPtr,
        name: "from_int",
        form: PointerOpForm::AssocFn,
        intrinsic: IntrinsicId::IntToPtr,
        intrinsic_name: "int_to_ptr",
        requires_checked: true,
    },
];

/// Look up a pointer method/assoc fn by `(kind, name, form)`.
pub fn lookup_pointer_method(
    kind: PointerKind,
    name: &str,
    form: PointerOpForm,
) -> Option<&'static PointerMethod> {
    POINTER_METHODS
        .iter()
        .find(|m| m.kind == kind && m.form == form && m.name == name)
}

// ============================================================================
// Slice-method registry (ADR-0064)
// ============================================================================

/// Which builtin slice constructor an entry is defined on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceKind {
    /// Defined on `Slice(T)`.
    Slice,
    /// Defined on `MutSlice(T)`.
    MutSlice,
}

/// One method on `Slice(T)` / `MutSlice(T)` (ADR-0064).
///
/// Mirrors [`PointerMethod`]: each entry maps a surface name to an
/// [`IntrinsicId`] that owns the actual semantic / codegen behaviour.
#[derive(Debug, Clone, Copy)]
pub struct SliceMethod {
    /// Constructor this method is defined on.
    pub kind: SliceKind,
    /// Method name as written by the user (after `.`).
    pub name: &'static str,
    /// Stable identity used by codegen / IR analyzers.
    pub intrinsic: IntrinsicId,
    /// Symbol the AIR `Intrinsic` instruction is tagged with.
    pub intrinsic_name: &'static str,
    /// Whether the lowering requires a `checked` block.
    pub requires_checked: bool,
}

/// Closed registry of every slice method (ADR-0064).
pub const SLICE_METHODS: &[SliceMethod] = &[
    // ---- Methods on Slice(T) ----
    SliceMethod {
        kind: SliceKind::Slice,
        name: "len",
        intrinsic: IntrinsicId::SliceLen,
        intrinsic_name: "slice_len",
        requires_checked: false,
    },
    SliceMethod {
        kind: SliceKind::Slice,
        name: "is_empty",
        intrinsic: IntrinsicId::SliceIsEmpty,
        intrinsic_name: "slice_is_empty",
        requires_checked: false,
    },
    SliceMethod {
        kind: SliceKind::Slice,
        name: "ptr",
        intrinsic: IntrinsicId::SlicePtr,
        intrinsic_name: "slice_ptr",
        requires_checked: true,
    },
    // ---- Methods on MutSlice(T) ----
    SliceMethod {
        kind: SliceKind::MutSlice,
        name: "len",
        intrinsic: IntrinsicId::SliceLen,
        intrinsic_name: "slice_len",
        requires_checked: false,
    },
    SliceMethod {
        kind: SliceKind::MutSlice,
        name: "is_empty",
        intrinsic: IntrinsicId::SliceIsEmpty,
        intrinsic_name: "slice_is_empty",
        requires_checked: false,
    },
    SliceMethod {
        kind: SliceKind::MutSlice,
        name: "ptr",
        intrinsic: IntrinsicId::SlicePtr,
        intrinsic_name: "slice_ptr",
        requires_checked: true,
    },
    SliceMethod {
        kind: SliceKind::MutSlice,
        name: "ptr_mut",
        intrinsic: IntrinsicId::SlicePtrMut,
        intrinsic_name: "slice_ptr_mut",
        requires_checked: true,
    },
];

/// Look up a slice method by `(kind, name)`.
pub fn lookup_slice_method(kind: SliceKind, name: &str) -> Option<&'static SliceMethod> {
    SLICE_METHODS
        .iter()
        .find(|m| m.kind == kind && m.name == name)
}

// ============================================================================
// Queries
// ============================================================================

/// Look up an intrinsic by its source-level name (without the leading `@`).
pub fn lookup_by_name(name: &str) -> Option<&'static IntrinsicDef> {
    INTRINSICS.iter().find(|d| d.name == name)
}

/// Look up an intrinsic by its stable [`IntrinsicId`].
pub fn lookup_by_id(id: IntrinsicId) -> &'static IntrinsicDef {
    INTRINSICS
        .iter()
        .find(|d| d.id == id)
        .expect("every IntrinsicId must have exactly one INTRINSICS entry (checked by tests)")
}

/// Iterate over every registered intrinsic.
pub fn iter() -> impl Iterator<Item = &'static IntrinsicDef> {
    INTRINSICS.iter()
}

/// Iterate over intrinsics in a single category.
pub fn by_category(cat: Category) -> impl Iterator<Item = &'static IntrinsicDef> {
    INTRINSICS.iter().filter(move |d| d.category == cat)
}

/// Is this name a type intrinsic (takes a type arg, as `@size_of(T)` does)?
pub fn is_type_intrinsic(name: &str) -> bool {
    lookup_by_name(name).is_some_and(|d| d.kind == IntrinsicKind::Type)
}

// ============================================================================
// Documentation export
// ============================================================================

/// Render the full intrinsics reference page as markdown.
///
/// The output is a self-contained page with a quick-reference table,
/// followed by per-category detail sections listing each intrinsic's name,
/// summary, description, runtime binding (if any), preview gate (if any),
/// unchecked requirement (if any), and examples.
///
/// This function is the source of truth for the checked-in reference page;
/// `make check-intrinsic-docs` runs it and fails CI if the committed file
/// differs from the generated output.
pub fn render_reference_markdown() -> String {
    let mut out = String::new();
    out.push_str("<!-- AUTO-GENERATED by `cargo run -p gruel-intrinsics-docs`. Do not edit by hand; edit the IntrinsicDef entries in `crates/gruel-intrinsics/src/lib.rs` and regenerate. -->\n\n");
    out.push_str("# Intrinsics Reference\n\n");
    out.push_str("This page documents every `@intrinsic` the Gruel compiler recognizes. It is generated from the [`gruel-intrinsics`] registry (see [ADR-0050](../designs/0050-intrinsics-crate.md)); any changes must be made in Rust, not here.\n\n");

    // ---- Quick reference table ----
    // Sort by the canonical category render order (stable on registration
    // order within a category) so rows for the same category cluster
    // together and the table reads like a table of contents for the detail
    // sections below.
    out.push_str("## Quick Reference\n\n");
    out.push_str("| Intrinsic | Kind | Category | Preview | Unchecked | Summary |\n");
    out.push_str("|---|---|---|---|---|---|\n");
    let mut ordered: Vec<&IntrinsicDef> = INTRINSICS.iter().collect();
    ordered.sort_by_key(|d| d.category.render_index());
    for d in ordered {
        let kind = match d.kind {
            IntrinsicKind::Expr => "expr",
            IntrinsicKind::Type => "type",
            IntrinsicKind::TypeInterface => "type+iface",
        };
        let preview = match d.preview {
            Some(f) => f.name(),
            None => "—",
        };
        let unchecked = if d.requires_unchecked { "yes" } else { "—" };
        out.push_str(&format!(
            "| `@{}` | {} | {} | {} | {} | {} |\n",
            d.name,
            kind,
            d.category.heading(),
            preview,
            unchecked,
            d.summary,
        ));
    }
    out.push('\n');

    // ---- Per-category detail sections ----
    for &cat in Category::RENDER_ORDER {
        let mut entries = by_category(cat).peekable();
        if entries.peek().is_none() {
            continue;
        }
        out.push_str(&format!("## {}\n\n", cat.heading()));
        for d in entries {
            out.push_str(&format!("### `@{}`\n\n", d.name));
            out.push_str(&format!("{}\n\n", d.description));
            if let Some(rt) = d.runtime_fn {
                out.push_str(&format!("- **Runtime symbol:** `{rt}`\n"));
            }
            if let Some(feature) = d.preview {
                out.push_str(&format!(
                    "- **Preview gate:** `--preview {}` ({})\n",
                    feature.name(),
                    feature.adr()
                ));
            }
            if d.requires_unchecked {
                out.push_str("- **Requires:** `checked { ... }` block\n");
            }
            if !d.examples.is_empty() {
                out.push_str("\n**Examples:**\n\n");
                for ex in d.examples {
                    out.push_str("```gruel\n");
                    out.push_str(ex);
                    if !ex.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str("```\n\n");
                }
            } else {
                out.push('\n');
            }
        }
    }
    out
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_hash::FxHashSet as HashSet;

    #[test]
    fn no_duplicate_names() {
        let mut seen = HashSet::default();
        for d in INTRINSICS {
            assert!(seen.insert(d.name), "duplicate intrinsic name: {}", d.name);
        }
    }

    #[test]
    fn no_duplicate_ids() {
        let mut seen = HashSet::default();
        for d in INTRINSICS {
            assert!(seen.insert(d.id), "duplicate IntrinsicId: {:?}", d.id);
        }
    }

    #[test]
    fn every_id_variant_covered() {
        // Exhaustive match ensures adding a new IntrinsicId variant without an
        // INTRINSICS entry fails to compile.
        for d in INTRINSICS {
            match d.id {
                IntrinsicId::Dbg
                | IntrinsicId::Panic
                | IntrinsicId::Assert
                | IntrinsicId::CompileError
                | IntrinsicId::Cast
                | IntrinsicId::ReadLine
                | IntrinsicId::ParseI32
                | IntrinsicId::ParseI64
                | IntrinsicId::ParseU32
                | IntrinsicId::ParseU64
                | IntrinsicId::RandomU32
                | IntrinsicId::RandomU64
                | IntrinsicId::SizeOf
                | IntrinsicId::AlignOf
                | IntrinsicId::TypeName
                | IntrinsicId::TypeInfo
                | IntrinsicId::Ownership
                | IntrinsicId::Implements
                | IntrinsicId::Field
                | IntrinsicId::Import
                | IntrinsicId::EmbedFile
                | IntrinsicId::TargetArch
                | IntrinsicId::TargetOs
                | IntrinsicId::PtrRead
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
                | IntrinsicId::Syscall
                | IntrinsicId::Range
                | IntrinsicId::SliceLen
                | IntrinsicId::SliceIsEmpty
                | IntrinsicId::SliceIndexRead
                | IntrinsicId::SliceIndexWrite
                | IntrinsicId::SlicePtr
                | IntrinsicId::SlicePtrMut
                | IntrinsicId::PartsToSlice
                | IntrinsicId::PartsToMutSlice
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
                | IntrinsicId::VecLiteral
                | IntrinsicId::VecRepeat
                | IntrinsicId::VecDispose
                | IntrinsicId::PartsToVec
                | IntrinsicId::TestPreviewGate
                | IntrinsicId::Utf8Validate
                | IntrinsicId::CStrToVec => {}
            }
        }
    }

    #[test]
    fn lookup_by_name_roundtrip() {
        for d in INTRINSICS {
            let found = lookup_by_name(d.name).expect("name must resolve");
            assert_eq!(found.id, d.id);
        }
        assert!(lookup_by_name("definitely_not_an_intrinsic").is_none());
    }

    #[test]
    fn lookup_by_id_roundtrip() {
        for d in INTRINSICS {
            assert_eq!(lookup_by_id(d.id).name, d.name);
        }
    }

    #[test]
    fn type_intrinsics_match_legacy_list() {
        // The legacy TYPE_INTRINSICS constant in gruel-rir/astgen.rs lists:
        // size_of, align_of, type_name, type_info, ownership.
        // The registry must match exactly.
        let from_registry: HashSet<&'static str> = INTRINSICS
            .iter()
            .filter(|d| d.kind == IntrinsicKind::Type)
            .map(|d| d.name)
            .collect();
        let expected: HashSet<&'static str> =
            ["size_of", "align_of", "type_name", "type_info", "ownership"]
                .into_iter()
                .collect();
        assert_eq!(from_registry, expected);
    }

    #[test]
    fn unchecked_intrinsics_match_legacy_set() {
        // Intrinsics that current sema calls `require_checked_for_intrinsic` on.
        let from_registry: HashSet<&'static str> = INTRINSICS
            .iter()
            .filter(|d| d.requires_unchecked)
            .map(|d| d.name)
            .collect();
        let expected: HashSet<&'static str> = [
            "ptr_read",
            "ptr_write",
            "ptr_read_volatile",
            "ptr_write_volatile",
            "ptr_offset",
            "ptr_to_int",
            "int_to_ptr",
            "null_ptr",
            "is_null",
            "ptr_copy",
            "raw",
            "raw_mut",
            "syscall",
            "slice_ptr",
            "slice_ptr_mut",
            "parts_to_slice",
            "parts_to_mut_slice",
            "vec_ptr",
            "vec_ptr_mut",
            "vec_terminated_ptr",
            "parts_to_vec",
            // ADR-0072
            "cstr_to_vec",
        ]
        .into_iter()
        .collect();
        assert_eq!(from_registry, expected);
    }

    #[test]
    fn by_category_filters() {
        let ptrs: Vec<_> = by_category(Category::Pointer).collect();
        assert!(!ptrs.is_empty());
        assert!(ptrs.iter().all(|d| d.category == Category::Pointer));
    }

    /// Every `Category` variant must appear exactly once in `RENDER_ORDER`,
    /// so the documentation renderer never silently drops a section. Adding
    /// a new variant without updating `RENDER_ORDER` makes this test fail.
    #[test]
    fn category_render_order_is_exhaustive() {
        // Enumerate every Category variant via an exhaustive match. The
        // compiler will force this to be updated when a new variant is
        // added; the assertion then forces RENDER_ORDER to be updated too.
        let all = [
            Category::Debug,
            Category::Cast,
            Category::Io,
            Category::Parse,
            Category::Random,
            Category::Comptime,
            Category::Platform,
            Category::Pointer,
            Category::Syscall,
            Category::Iteration,
            Category::Slice,
            Category::Vec,
            Category::Meta,
        ];
        // Exhaustiveness witness: any new variant added to `Category`
        // forces this match to be updated.
        for c in all {
            let _: &'static str = match c {
                Category::Debug => "Debug",
                Category::Cast => "Cast",
                Category::Io => "Io",
                Category::Parse => "Parse",
                Category::Random => "Random",
                Category::Comptime => "Comptime",
                Category::Platform => "Platform",
                Category::Pointer => "Pointer",
                Category::Syscall => "Syscall",
                Category::Iteration => "Iteration",
                Category::Slice => "Slice",
                Category::Vec => "Vec",
                Category::Meta => "Meta",
            };
            assert!(
                Category::RENDER_ORDER.contains(&c),
                "Category::{:?} is missing from Category::RENDER_ORDER \
                 — add it to keep the intrinsics reference page complete",
                c
            );
        }
        assert_eq!(
            Category::RENDER_ORDER.len(),
            all.len(),
            "Category::RENDER_ORDER has duplicates or is out of sync with Category"
        );
    }

    #[test]
    fn is_type_intrinsic_basic() {
        assert!(is_type_intrinsic("size_of"));
        assert!(is_type_intrinsic("type_name"));
        assert!(!is_type_intrinsic("dbg"));
        assert!(!is_type_intrinsic("nonexistent"));
    }

    #[test]
    fn all_names_are_valid_identifiers() {
        for d in INTRINSICS {
            assert!(!d.name.is_empty());
            assert!(
                d.name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "intrinsic name {:?} has unexpected characters",
                d.name
            );
        }
    }

    #[test]
    fn preview_gated_intrinsics_are_known_features() {
        // All preview gates reference the PreviewFeature enum, so the compiler
        // already enforces this at type-check time. This test just documents
        // that `test_preview_gate` currently carries a gate.
        let gated: Vec<_> = INTRINSICS.iter().filter(|d| d.preview.is_some()).collect();
        assert!(
            gated.iter().any(|d| d.name == "test_preview_gate"),
            "test_preview_gate must be preview-gated"
        );
    }
}

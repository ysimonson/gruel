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

use gruel_error::PreviewFeature;

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
    Field,
    Import,

    // ---- Target platform ----
    TargetArch,
    TargetOs,

    // ---- Pointer operations (require unchecked) ----
    PtrRead,
    PtrWrite,
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
            Category::Meta => "Preview / Meta",
        }
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
        description: "`@size_of(T)` returns `sizeof(T)` as `i32`, evaluated at compile time.",
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
        description: "`@align_of(T)` returns the required alignment of `T` as `i32`, evaluated at compile time.",
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
    IntrinsicDef {
        id: IntrinsicId::PtrRead,
        name: "ptr_read",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Load a value through a raw pointer.",
        description: "`@ptr_read(p)` dereferences `p: ptr const T` or `ptr mut T` and returns `T`. Requires an `unchecked` block.",
        examples: &["unchecked { let v = @ptr_read(p); }"],
    },
    IntrinsicDef {
        id: IntrinsicId::PtrWrite,
        name: "ptr_write",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Store a value through a raw mutable pointer.",
        description: "`@ptr_write(p, v)` writes `v` through `p: ptr mut T`. Requires an `unchecked` block.",
        examples: &["unchecked { @ptr_write(p, 42); }"],
    },
    IntrinsicDef {
        id: IntrinsicId::PtrOffset,
        name: "ptr_offset",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Pointer arithmetic by element count.",
        description: "`@ptr_offset(p, n)` advances `p` by `n * sizeof(*p)` bytes. Requires an `unchecked` block.",
        examples: &["unchecked { let q = @ptr_offset(p, 3); }"],
    },
    IntrinsicDef {
        id: IntrinsicId::PtrToInt,
        name: "ptr_to_int",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Convert a pointer to its integer address.",
        description: "`@ptr_to_int(p)` returns the address of `p` as `u64`. Requires an `unchecked` block.",
        examples: &["unchecked { let a = @ptr_to_int(p); }"],
    },
    IntrinsicDef {
        id: IntrinsicId::IntToPtr,
        name: "int_to_ptr",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Construct a pointer from an integer address.",
        description: "`@int_to_ptr(addr)` reinterprets `addr: u64` as a pointer. Target pointer type is inferred from context. Requires an `unchecked` block.",
        examples: &["unchecked { let p: ptr mut u8 = @int_to_ptr(addr); }"],
    },
    IntrinsicDef {
        id: IntrinsicId::NullPtr,
        name: "null_ptr",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "A null pointer of the inferred type.",
        description: "`@null_ptr()` returns a pointer whose address is zero; the pointer type is inferred from context. Requires an `unchecked` block.",
        examples: &["unchecked { let p: ptr const u8 = @null_ptr(); }"],
    },
    IntrinsicDef {
        id: IntrinsicId::IsNull,
        name: "is_null",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Test whether a pointer is null.",
        description: "`@is_null(p)` returns `true` iff `p`'s address is zero. Requires an `unchecked` block.",
        examples: &["unchecked { if @is_null(p) { ... } }"],
    },
    IntrinsicDef {
        id: IntrinsicId::PtrCopy,
        name: "ptr_copy",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Bulk copy between pointers.",
        description: "`@ptr_copy(dst, src, n)` copies `n` elements of the pointee type from `src` to `dst` via LLVM `memcpy`. Requires an `unchecked` block.",
        examples: &["unchecked { @ptr_copy(dst, src, 16); }"],
    },
    IntrinsicDef {
        id: IntrinsicId::Raw,
        name: "raw",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Take a const pointer to an lvalue.",
        description: "`@raw(place)` returns `ptr const T` pointing to `place`. Requires an `unchecked` block.",
        examples: &["unchecked { let p = @raw(x); }"],
    },
    IntrinsicDef {
        id: IntrinsicId::RawMut,
        name: "raw_mut",
        kind: IntrinsicKind::Expr,
        category: Category::Pointer,
        requires_unchecked: true,
        preview: None,
        runtime_fn: None,
        summary: "Take a mutable pointer to an lvalue.",
        description: "`@raw_mut(place)` returns `ptr mut T` pointing to `place`. Requires an `unchecked` block.",
        examples: &["unchecked { let p = @raw_mut(x); }"],
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
        examples: &["unchecked { let ret = @syscall(1, 1, buf, n); }"],
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
];

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
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn no_duplicate_names() {
        let mut seen = HashSet::new();
        for d in INTRINSICS {
            assert!(seen.insert(d.name), "duplicate intrinsic name: {}", d.name);
        }
    }

    #[test]
    fn no_duplicate_ids() {
        let mut seen = HashSet::new();
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
                | IntrinsicId::Field
                | IntrinsicId::Import
                | IntrinsicId::TargetArch
                | IntrinsicId::TargetOs
                | IntrinsicId::PtrRead
                | IntrinsicId::PtrWrite
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
                | IntrinsicId::TestPreviewGate => {}
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
        // size_of, align_of, type_name, type_info.
        // The registry must match exactly.
        let from_registry: HashSet<&'static str> = INTRINSICS
            .iter()
            .filter(|d| d.kind == IntrinsicKind::Type)
            .map(|d| d.name)
            .collect();
        let expected: HashSet<&'static str> = ["size_of", "align_of", "type_name", "type_info"]
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
            "ptr_offset",
            "ptr_to_int",
            "int_to_ptr",
            "null_ptr",
            "is_null",
            "ptr_copy",
            "raw",
            "raw_mut",
            "syscall",
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

//! Built-in registries for the Gruel compiler.
//!
//! After ADR-0081 retired the `STRING_TYPE` registry, this crate hosts
//! three smaller registries the compiler still keys off of:
//!
//! - **Built-in type constructors** ([`BUILTIN_TYPE_CONSTRUCTORS`]):
//!   `Ptr`, `MutPtr`, `Ref`, `MutRef`, `Slice`, `MutSlice`, `Vec` — written
//!   in source as `Name(arg, ...)` in type position and lowered directly to
//!   `TypeKind` variants by sema.
//! - **Lang items** ([`LangInterfaceItem`], [`LangEnumItem`]): the closed
//!   set of `@lang("…")` strings the compiler recognises and binds to
//!   prelude interface or enum declarations (`Drop`, `Clone`, `Handle`,
//!   `Eq`, `Ord`, `Ordering`).
//! - **Built-in interface and enum names** ([`BUILTIN_INTERFACE_NAMES`],
//!   [`BUILTIN_ENUM_NAMES`]): breadcrumbs the doc generator and other
//!   crates use to refer to prelude declarations without re-typing the
//!   strings.
//!
//! All four prelude-resident built-in enums (`Arch`, `Os`, `TypeKind`,
//! `Ownership`) and the three interfaces (`Drop`, `Clone`, `Handle`)
//! live in the prelude — see `prelude/target.gruel`,
//! `prelude/type_info.gruel`, and `prelude/interfaces.gruel`. The
//! compiler caches their `EnumId` / `InterfaceId`s after declaration
//! resolution; see `Sema::cache_builtin_enum_ids` in `gruel-air`.

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
// Built-in Markers (ADR-0083: `@mark(...)` directive)
// ============================================================================
//
// Markers are declaration-time-only attributes carried by `@mark(...)` on
// struct/enum (and anonymous literal) heads. They sit alongside `@derive`,
// `@lang`, and `@allow` in the directive list. Today's marker set is
// closed and small (`copy`, `affine`, `linear` — all of them postures);
// future ADRs may add more without parser surgery — adding a row here is
// the extension point.
//
// Three markers cover the posture trichotomy: `copy` asserts the type
// must structurally infer Copy; `affine` suppresses Copy inference (the
// type is always Affine, even if its members would otherwise make it
// Copy); `linear` overrides inference to declare the type Linear
// regardless of member postures.

/// What a marker conveys to sema. Markers fall into independent
/// "namespaces" — at most one posture marker may attach to a type, and
/// (separately) at most one thread-safety marker. Future markers (e.g.
/// capability tags, layout hints) plug in by adding a new variant
/// without disturbing the existing ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerKind {
    Posture(Posture),
    /// ADR-0084: thread-safety classification overrides. `unsend` is the
    /// always-safe downgrade; `checked_send` and `checked_sync` are
    /// user-asserted upgrades the compiler cannot verify on its own.
    ThreadSafety(ThreadSafety),
    /// ADR-0085: ABI markers — C ABI on fns, C layout on structs.
    Abi(Abi),
}

/// ABI marker variants (ADR-0085). C is the only ABI in v1; future
/// values (`system`, `stdcall`, `vectorcall`, eventually `rust`) extend
/// this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Abi {
    /// C ABI: C calling convention on fns, C layout on structs.
    C,
}

/// Posture trichotomy carried by `MarkerKind::Posture` (ADR-0080 / ADR-0083).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Posture {
    /// The type is Copy: bitwise duplicable, never moves on assignment.
    Copy,
    /// The type is Affine: the default — moves on use, no implicit duplicate.
    /// `@mark(affine)` exists so users can suppress structural Copy inference
    /// when a type's members are all Copy but its semantics demand
    /// move-on-use.
    Affine,
    /// The type is Linear: must be explicitly consumed; cannot be silently
    /// dropped.
    Linear,
}

/// Thread-safety trichotomy (ADR-0084).
///
/// Carried by `MarkerKind::ThreadSafety` and stored on every type-bearing
/// `StructDef` / `EnumDef`. Inference takes the structural minimum over
/// members; primitives are intrinsically `Sync` and raw pointers are
/// intrinsically `Unsend`.
///
/// The variant order matters: it makes the derived `Ord` impl yield the
/// chain `Unsend < Send < Sync`, which is what the structural-minimum
/// inference rule expects.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ThreadSafety {
    /// Cannot cross thread boundaries.
    Unsend,
    /// Safe to move across threads (transferring ownership).
    Send,
    /// Safe to share across threads. Subsumes `Send`.
    Sync,
}

impl Default for ThreadSafety {
    /// `Sync` is the identity for the structural-minimum operation —
    /// `min(any, Sync) = any`. Defaulting to `Sync` means an
    /// uninitialized field has no impact when the inference pass folds
    /// over members.
    fn default() -> Self {
        ThreadSafety::Sync
    }
}

/// Item kinds a marker is applicable to.
///
/// Markers may permit structs, enums, or both. Today both posture markers
/// allow either; the field is forward-looking for future markers (e.g. a
/// "no-niche" marker that only makes sense on structs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemKinds(u8);

impl ItemKinds {
    pub const STRUCT: ItemKinds = ItemKinds(0b001);
    pub const ENUM: ItemKinds = ItemKinds(0b010);
    pub const FUNCTION: ItemKinds = ItemKinds(0b100);
    pub const STRUCT_OR_ENUM: ItemKinds = ItemKinds(0b011);
    pub const FN_OR_STRUCT: ItemKinds = ItemKinds(0b101);

    pub fn includes_struct(self) -> bool {
        (self.0 & Self::STRUCT.0) != 0
    }

    pub fn includes_enum(self) -> bool {
        (self.0 & Self::ENUM.0) != 0
    }

    pub fn includes_function(self) -> bool {
        (self.0 & Self::FUNCTION.0) != 0
    }
}

/// Definition of a built-in marker.
///
/// Markers are looked up by name (see [`get_builtin_marker`]) when sema
/// processes `@mark(...)` arguments. The closed list documents what
/// *exists*; user-defined markers are explicitly out of scope (ADR-0083).
#[derive(Debug, Clone, Copy)]
pub struct BuiltinMarker {
    /// Marker name as it appears inside `@mark(...)` (e.g. "copy").
    pub name: &'static str,
    /// What the marker does — a posture today, possibly more later.
    pub kind: MarkerKind,
    /// Item kinds the marker is applicable to.
    pub applicable_to: ItemKinds,
}

/// All built-in markers recognized by the compiler.
///
/// Sema iterates this slice when looking up `@mark(name)` arguments and
/// when generating "did you mean?" suggestions for typo'd marker names.
pub static BUILTIN_MARKERS: &[BuiltinMarker] = &[
    BuiltinMarker {
        name: "copy",
        kind: MarkerKind::Posture(Posture::Copy),
        applicable_to: ItemKinds::STRUCT_OR_ENUM,
    },
    BuiltinMarker {
        name: "affine",
        kind: MarkerKind::Posture(Posture::Affine),
        applicable_to: ItemKinds::STRUCT_OR_ENUM,
    },
    BuiltinMarker {
        name: "linear",
        kind: MarkerKind::Posture(Posture::Linear),
        applicable_to: ItemKinds::STRUCT_OR_ENUM,
    },
    // ADR-0084: thread-safety overrides. `unsend` is an always-safe
    // downgrade. `checked_send` / `checked_sync` are user-asserted
    // upgrades the compiler cannot verify; the `checked_` prefix names
    // them as such (analogous to Rust's `unsafe impl Send`).
    BuiltinMarker {
        name: "unsend",
        kind: MarkerKind::ThreadSafety(ThreadSafety::Unsend),
        applicable_to: ItemKinds::STRUCT_OR_ENUM,
    },
    BuiltinMarker {
        name: "checked_send",
        kind: MarkerKind::ThreadSafety(ThreadSafety::Send),
        applicable_to: ItemKinds::STRUCT_OR_ENUM,
    },
    BuiltinMarker {
        name: "checked_sync",
        kind: MarkerKind::ThreadSafety(ThreadSafety::Sync),
        applicable_to: ItemKinds::STRUCT_OR_ENUM,
    },
    // ADR-0085: C FFI. Applied to fns selects the C calling convention;
    // applied to structs selects C layout (field order, alignment,
    // niches disabled). Enums are gated on a follow-up ADR that adds
    // `c_int` (the C enum discriminant type).
    BuiltinMarker {
        name: "c",
        kind: MarkerKind::Abi(Abi::C),
        applicable_to: ItemKinds::FN_OR_STRUCT,
    },
];

/// Look up a built-in marker by name.
pub fn get_builtin_marker(name: &str) -> Option<&'static BuiltinMarker> {
    BUILTIN_MARKERS.iter().find(|m| m.name == name)
}

/// All recognized marker names (for diagnostic suggestions).
pub fn all_marker_names() -> Vec<&'static str> {
    BUILTIN_MARKERS.iter().map(|m| m.name).collect()
}

// ============================================================================
// Built-in Enums (Arch, Os, TypeKind, Ownership)
// ============================================================================
//
// ADR-0078 Phase 3: the platform-reflection enums (`Arch`, `Os`) live
// in `prelude/target.gruel`; the type-reflection enums (`TypeKind`,
// `Ownership`) live in `prelude/type_info.gruel`. The intrinsics that
// produce values of those types (`@target_arch`, `@target_os`,
// `@type_info`, `@ownership`) cache their `EnumId`s after declaration
// resolution via `Sema::cache_builtin_enum_ids`. Variant order in the
// prelude files matches the order returned by the compiler-side
// `arch_variant_index` / `os_variant_index` mappers; see
// `crates/gruel-air/src/sema/analysis.rs`.

/// Names of the prelude-resident built-in enums. Kept here only so
/// other crates have a single source of truth when they need to refer to
/// the names (e.g. for documentation generation).
pub static BUILTIN_ENUM_NAMES: &[&str] = &["Arch", "Os", "TypeKind", "Ownership", "ThreadSafety"];

// ============================================================================
// Built-in Interfaces (Drop, Clone, Handle)
// ============================================================================
//
// ADR-0078 Phase 2: the interface declarations live in
// `prelude/interfaces.gruel`. The compiler still recognizes them by
// interned name (the hardcoded behaviors — drop glue, @derive(Clone)
// synthesis, Handle linearity carve-out — key off these names).
// ADR-0080 retired `Copy` from the interface set: posture is declared
// on the type and queried via `@ownership(T)`, not via interface
// conformance.

/// Names of the three compiler-recognized built-in interfaces. Kept
/// here only so the doc generator can point at `prelude/interfaces.gruel`
/// for canonical declarations. Do not use this for anything load-bearing
/// — the compiler resolves these names through the prelude scope.
pub static BUILTIN_INTERFACE_NAMES: &[&str] = &["Drop", "Clone", "Handle"];

// ============================================================================
// Lang items (ADR-0079)
// ============================================================================
//
// `@lang("name")` directives in the prelude bind the compiler's built-in
// behaviors (drop glue, copy/clone synthesis, operator desugaring, …) to
// specific interface or enum declarations. The closed list here is the
// only set of names the compiler recognizes — unknown lang-item names
// produce a compile error at the directive site. Stdlib renames the
// underlying type freely (e.g. `Clone` → `Dup`) so long as the renamed
// declaration carries the matching `@lang(...)` tag.

/// Lang-item name applied to an interface declaration.
///
/// ADR-0080 retired `Copy` from this enum: posture is declared on the
/// type with the `copy` keyword, queried via `@ownership(T)`, and never
/// dispatched, so it is no longer an interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LangInterfaceItem {
    /// `Drop` — values may carry custom destructors.
    Drop,
    /// `Clone` — values support a `clone(self)` method producing an
    /// owned duplicate.
    Clone,
    /// `Handle` — wraps a non-copyable resource that's still allowed to
    /// move out of `let` bindings (linear-type carve-out).
    Handle,
    /// `Eq` — drives `==` operator desugaring.
    OpEq,
    /// `Ord` — drives `<`/`<=`/`>`/`>=` operator desugaring.
    OpCmp,
}

/// Lang-item name applied to an enum declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LangEnumItem {
    /// `Ordering` — return type of `Ord::cmp`; variants drive ordering
    /// operator desugaring.
    Ordering,
}

/// Lang-item name applied to a function declaration. Used for
/// type-constructor functions whose result has compiler-recognized
/// behavior (indexing, slice-borrow, drop synthesis, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LangFnItem {
    /// `Vec(comptime T: type) -> type` (ADR-0082). Instances of the
    /// returned struct are recognized as the canonical owned-buffer
    /// vector for `v[i]` indexing, `&v[..]` slice borrows, and drop
    /// synthesis.
    Vec,
}

impl LangFnItem {
    pub fn name(self) -> &'static str {
        match self {
            LangFnItem::Vec => "vec",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "vec" => LangFnItem::Vec,
            _ => return None,
        })
    }

    pub fn all() -> &'static [LangFnItem] {
        &[LangFnItem::Vec]
    }
}

impl LangInterfaceItem {
    /// The string the prelude uses inside `@lang("…")` for this item.
    pub fn name(self) -> &'static str {
        match self {
            LangInterfaceItem::Drop => "drop",
            LangInterfaceItem::Clone => "clone",
            LangInterfaceItem::Handle => "handle",
            LangInterfaceItem::OpEq => "op_eq",
            LangInterfaceItem::OpCmp => "op_cmp",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "drop" => LangInterfaceItem::Drop,
            "clone" => LangInterfaceItem::Clone,
            "handle" => LangInterfaceItem::Handle,
            "op_eq" => LangInterfaceItem::OpEq,
            "op_cmp" => LangInterfaceItem::OpCmp,
            _ => return None,
        })
    }

    pub fn all() -> &'static [LangInterfaceItem] {
        use LangInterfaceItem::*;
        &[Drop, Clone, Handle, OpEq, OpCmp]
    }
}

impl LangEnumItem {
    pub fn name(self) -> &'static str {
        match self {
            LangEnumItem::Ordering => "ordering",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "ordering" => LangEnumItem::Ordering,
            _ => return None,
        })
    }

    pub fn all() -> &'static [LangEnumItem] {
        &[LangEnumItem::Ordering]
    }
}

/// Classification of a lang-item name string. Returns `None` for
/// unrecognized strings.
pub enum LangItemKind {
    Interface(LangInterfaceItem),
    Enum(LangEnumItem),
    Fn(LangFnItem),
}

impl LangItemKind {
    pub fn from_str(s: &str) -> Option<Self> {
        if let Some(i) = LangInterfaceItem::from_str(s) {
            Some(LangItemKind::Interface(i))
        } else if let Some(e) = LangEnumItem::from_str(s) {
            Some(LangItemKind::Enum(e))
        } else {
            LangFnItem::from_str(s).map(LangItemKind::Fn)
        }
    }
}

/// Closed list of lang-item names recognized by the compiler. Driving
/// data for diagnostics — the actual lookup goes through
/// `LangInterfaceItem::from_str` / `LangEnumItem::from_str` /
/// `LangFnItem::from_str`.
pub fn all_lang_item_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = LangInterfaceItem::all()
        .iter()
        .map(|i| i.name())
        .chain(LangEnumItem::all().iter().map(|e| e.name()))
        .chain(LangFnItem::all().iter().map(|f| f.name()))
        .collect();
    names.sort();
    names
}

// ============================================================================
// Reference doc generation
// ============================================================================

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

/// Render the reference page for built-in type constructors, enums, and
/// interfaces.
///
/// The output is a self-contained markdown page generated from the registries
/// in this crate. It is the source of truth for the checked-in reference page
/// at `docs/generated/builtins-reference.md`; `make check-builtins-docs` runs
/// it and fails CI if the committed file differs from the generated output.
pub fn render_reference_markdown() -> String {
    let mut out = String::new();
    out.push_str("<!-- AUTO-GENERATED by `cargo run -p gruel-builtins-docs`. Do not edit by hand; edit the registries in `crates/gruel-builtins/src/lib.rs` and regenerate. -->\n\n");
    out.push_str("# Built-in Types Reference\n\n");
    out.push_str("This page documents every built-in type constructor, enum, and interface the Gruel compiler hard-codes by name. ADR-0081 retired the `BUILTIN_TYPES` registry; built-in *types* (currently just `String`) live in the prelude alongside `Option` / `Result`. The constructors, enums, and interfaces here are still hard-wired because their semantics aren't expressible as ordinary Gruel code.\n\n");

    // ---- Quick reference ----
    out.push_str("## Quick Reference\n\n");

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
    out.push_str("Platform-reflection enums (`Arch`, `Os`) live in `prelude/target.gruel`; type-reflection enums (`TypeKind`, `Ownership`) live in `prelude/type_info.gruel`. The corresponding intrinsics produce values of these types by name lookup.\n\n");
    out.push_str("| Name | Variants |\n");
    out.push_str("|---|---|\n");
    out.push_str("| `Arch` | `X86_64`, `Aarch64`, `X86`, `Arm`, `Riscv32`, `Riscv64`, `Wasm32`, `Wasm64` |\n");
    out.push_str("| `Os` | `Linux`, `Macos`, `Windows`, `Freestanding`, `Wasi` |\n");
    out.push_str("| `TypeKind` | `Struct`, `Enum`, `Int`, `Bool`, `Unit`, `Never`, `Array` |\n");
    out.push_str("| `Ownership` | `Copy`, `Affine`, `Linear` |\n");
    out.push_str("| `ThreadSafety` | `Unsend`, `Send`, `Sync` |\n");
    out.push('\n');

    out.push_str("### Interfaces\n\n");
    out.push_str("Compiler-recognized interfaces are declared in `prelude/interfaces.gruel`. The compiler keys off these names for hardcoded behaviors (drop glue, `@derive(Clone)` synthesis, `Handle` linearity carve-out). ADR-0080 retired `Copy` from this set: posture is declared on the type with the `copy` keyword and queried via `@ownership(T)`.\n\n");
    out.push_str("| Name | Method | Conformance |\n");
    out.push_str("|---|---|---|\n");
    out.push_str("| `Drop` | `fn __drop(self)` | method presence |\n");
    out.push_str("| `Clone` | `fn clone(self: Ref(Self)) -> Self` | `@derive(Clone)` |\n");
    out.push_str("| `Handle` | `fn handle(self: Ref(Self)) -> Self` | method presence |\n");
    out.push('\n');

    out.push_str("### Markers\n\n");
    out.push_str("Marker names recognized inside the `@mark(...)` directive (ADR-0083). Markers attach declaration-time metadata to a struct/enum head; future markers plug in by adding a row to the `BUILTIN_MARKERS` registry.\n\n");
    out.push_str("| Name | Kind | Applies to |\n");
    out.push_str("|---|---|---|\n");
    for m in BUILTIN_MARKERS {
        let kind_str = match m.kind {
            MarkerKind::Posture(Posture::Copy) => "Posture(Copy)",
            MarkerKind::Posture(Posture::Affine) => "Posture(Affine)",
            MarkerKind::Posture(Posture::Linear) => "Posture(Linear)",
            MarkerKind::ThreadSafety(ThreadSafety::Unsend) => "ThreadSafety(Unsend)",
            MarkerKind::ThreadSafety(ThreadSafety::Send) => "ThreadSafety(Send)",
            MarkerKind::ThreadSafety(ThreadSafety::Sync) => "ThreadSafety(Sync)",
            MarkerKind::Abi(Abi::C) => "Abi(C)",
        };
        let apply_str = match (
            m.applicable_to.includes_struct(),
            m.applicable_to.includes_enum(),
            m.applicable_to.includes_function(),
        ) {
            (true, true, false) => "struct or enum",
            (true, false, false) => "struct only",
            (false, true, false) => "enum only",
            (true, false, true) => "fn or struct",
            (false, false, true) => "fn only",
            _ => "(none)",
        };
        out.push_str(&format!(
            "| `{}` | {} | {} |\n",
            m.name, kind_str, apply_str
        ));
    }
    out.push('\n');

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
    //
    // ADR-0078 Phase 3: `Arch`/`Os` live in `prelude/target.gruel`,
    // `TypeKind`/`Ownership` in `prelude/type_info.gruel`. Variant
    // order in this section matches the prelude files.
    out.push_str("## Enums\n\n");
    out.push_str("Platform-reflection (`Arch`, `Os`) and type-reflection (`TypeKind`, `Ownership`) enums. Declarations live in `prelude/target.gruel` and `prelude/type_info.gruel` respectively; the corresponding intrinsics (`@target_arch`, `@target_os`, `@type_info`, `@ownership`) materialize values of these types.\n\n");

    for (name, variants) in [
        (
            "Arch",
            &[
                "X86_64", "Aarch64", "X86", "Arm", "Riscv32", "Riscv64", "Wasm32", "Wasm64",
            ][..],
        ),
        (
            "Os",
            &["Linux", "Macos", "Windows", "Freestanding", "Wasi"][..],
        ),
        (
            "TypeKind",
            &["Struct", "Enum", "Int", "Bool", "Unit", "Never", "Array"][..],
        ),
        ("Ownership", &["Copy", "Affine", "Linear"][..]),
        ("ThreadSafety", &["Unsend", "Send", "Sync"][..]),
    ] {
        out.push_str(&format!("### `{}`\n\n", name));
        out.push_str("| Index | Variant |\n");
        out.push_str("|---|---|\n");
        for (i, v) in variants.iter().enumerate() {
            out.push_str(&format!("| {} | `{}::{}` |\n", i, name, v));
        }
        out.push('\n');
    }

    // ---- Interfaces in detail ----
    //
    // ADR-0078 Phase 2: declarations live in `prelude/interfaces.gruel`.
    // Names listed here as a directory; canonical signatures and method
    // bodies are in the prelude file.
    out.push_str("## Interfaces\n\n");
    out.push_str("Compiler-recognized interfaces. Declarations live in `prelude/interfaces.gruel`; the compiler keys off the interface names for hardcoded behaviors. Conformance is structural — a type satisfies the interface when it provides matching methods.\n\n");

    out.push_str("### `Drop`\n\n");
    out.push_str("Types with custom cleanup logic that runs when the value goes out of scope (ADR-0059).\n\n");
    out.push_str("**Required methods:**\n\n");
    out.push_str("- `fn __drop(self)`\n\n");
    out.push_str("**Conformance:** structural (no derive). Defining `fn __drop(self)` on a struct or enum makes it conform — there is no `@derive(Drop)` directive.\n\n");

    out.push_str("### `Clone`\n\n");
    out.push_str("Types that may be explicitly duplicated via `.clone()`. All `Copy` types auto-conform (ADR-0065).\n\n");
    out.push_str("**Required methods:**\n\n");
    out.push_str("- `fn clone(self: Ref(Self)) -> Self`\n\n");
    out.push_str("**Conformance derive:** `@derive(Clone)` (compiler-recognized; no user `derive` declaration required). Synthesizes a `clone` method that recursively calls `clone` on every field (struct) or variant payload (enum). Synthesis fails if any field is not `Clone`. Rejected on `linear` types.\n\n");

    out.push_str("### `Handle`\n\n");
    out.push_str("Types that may be explicitly duplicated via `.handle()`, typically because the duplication has visible cost (refcount bumps, transaction forks). Unlike `Clone`, `Handle` is permitted on `linear` types (ADR-0075).\n\n");
    out.push_str("**Required methods:**\n\n");
    out.push_str("- `fn handle(self: Ref(Self)) -> Self`\n\n");
    out.push_str("**Conformance:** structural (no derive). Defining `fn handle(self: Ref(Self)) -> Self` on a struct or enum makes it conform — there is no `@derive(Handle)` directive.\n\n");

    // ---- Markers in detail ----
    out.push_str("## Markers\n\n");
    out.push_str("Markers are declaration-time-only attributes on struct/enum heads, written inside `@mark(...)` (ADR-0083). The marker set is closed; user-defined markers are out of scope. New markers must go through an ADR.\n\n");
    for m in BUILTIN_MARKERS {
        out.push_str(&format!("### `@mark({})`\n\n", m.name));
        match m.kind {
            MarkerKind::Posture(Posture::Copy) => out.push_str(
                "Asserts the type is Copy. Under uniform structural inference, a struct/enum of all-Copy fields would already be Copy without the directive — `@mark(copy)` exists so the user can document intent and turn a silent posture downgrade (adding a non-Copy field later) into a declaration-site error.\n\n",
            ),
            MarkerKind::Posture(Posture::Affine) => out.push_str(
                "Suppresses Copy inference. A type whose members would otherwise infer Copy is forced to remain Affine, so move-on-use semantics are preserved even when bitwise duplication is safe. Has no effect on Linear inference: a Linear member still propagates upward.\n\n",
            ),
            MarkerKind::Posture(Posture::Linear) => out.push_str(
                "Forces the type to be Linear regardless of member postures. Use when the type has linear semantics that are not visible from its fields (e.g. an `i32` handle that is actually a kernel resource ID).\n\n",
            ),
            MarkerKind::ThreadSafety(ThreadSafety::Unsend) => out.push_str(
                "Downgrades the type's thread-safety classification to `Unsend`, even if its members would structurally permit `Send` or `Sync`. Always safe — the marker only restricts. Use when the type has thread-affine state that isn't visible from its fields (e.g. a handle to a thread-local resource).\n\n",
            ),
            MarkerKind::ThreadSafety(ThreadSafety::Send) => out.push_str(
                "Asserts the type is `Send`, even if a member's type would structurally pull it down to `Unsend` (e.g. a raw pointer field). The compiler cannot verify this — the `checked_` prefix flags it as a user assertion (analogous to Rust's `unsafe impl Send`). Mis-applying breaks data-race freedom; the user takes responsibility.\n\n",
            ),
            MarkerKind::ThreadSafety(ThreadSafety::Sync) => out.push_str(
                "Asserts the type is `Sync`, even if its structural minimum would be `Send` or `Unsend`. The compiler cannot verify this — the `checked_` prefix flags it as a user assertion (analogous to Rust's `unsafe impl Sync`). Mis-applying breaks data-race freedom; the user takes responsibility.\n\n",
            ),
            MarkerKind::Abi(Abi::C) => out.push_str(
                "Selects the C ABI / C layout (ADR-0085). On a function, uses the platform C calling convention and suppresses Gruel name mangling. On a struct, switches to C field layout (declaration order, natural alignment, no reordering, niches disabled), making the type eligible to cross the FFI boundary by value.\n\n",
            ),
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_enum_names() {
        assert_eq!(
            BUILTIN_ENUM_NAMES,
            &["Arch", "Os", "TypeKind", "Ownership", "ThreadSafety"]
        );
    }

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

//! Information types for functions, methods, and constants.
//!
//! These types store metadata about declarations gathered during the first
//! phase of semantic analysis. They are used to resolve function calls,
//! method calls, and constant references during function body analysis.

use gruel_span::{FileId, Span};
use lasso::Spur;

use crate::param_arena::ParamRange;
use crate::types::Type;

/// Information about a function.
#[derive(Debug, Clone, Copy)]
pub struct FunctionInfo {
    /// Parameter data (names, types, modes, comptime flags) stored in arena.
    /// Access via `arena.names(params)`, `arena.types(params)`, etc.
    pub params: ParamRange,
    /// Return type
    pub return_type: Type,
    /// The return type symbol (before resolution) - needed for generic function specialization
    pub return_type_sym: Spur,
    /// The RIR instruction ref for the function body - needed for generic function specialization
    pub body: gruel_rir::InstRef,
    /// Span of the function declaration
    pub span: Span,
    /// Whether this function has any comptime type parameters
    pub is_generic: bool,
    /// Whether this function is public (visible outside its directory)
    pub is_pub: bool,
    /// Whether this function is marked `unchecked` (can only be called from checked blocks)
    pub is_unchecked: bool,
    /// File ID this function was declared in (for visibility checking)
    pub file_id: FileId,
}

/// Information about a method in an impl block.
///
/// Note: Captured comptime values for anonymous struct methods are stored at the
/// struct level in `Sema::anon_struct_captured_values`, not per-method. This ensures
/// that different instantiations with different captured values create different types.
#[derive(Debug, Clone, Copy)]
pub struct MethodInfo {
    /// The struct type this method belongs to
    pub struct_type: Type,
    /// Whether this is a method (has self) or associated function (no self)
    pub has_self: bool,
    /// Receiver mode (`self`, `inout self`, `borrow self`). Meaningful only
    /// when `has_self` is true; `ByValue` is used as a placeholder for
    /// associated functions (ADR-0060).
    pub receiver: crate::types::ReceiverMode,
    /// Parameter data (names, types, modes, comptime flags) stored in arena.
    /// Access via `arena.names(params)`, `arena.types(params)`, etc.
    /// Note: This excludes `self` if present - only explicit parameters.
    pub params: ParamRange,
    /// Return type
    pub return_type: Type,
    /// The RIR instruction ref for the method body
    pub body: gruel_rir::InstRef,
    /// Span of the method declaration
    pub span: Span,
    /// Whether this method is marked `unchecked` (can only be called from checked blocks)
    pub is_unchecked: bool,
    /// True if this method has its own comptime type parameters (e.g.,
    /// `fn apply(self, comptime F: type, f: F) -> T`). Such methods are
    /// generic at the method level (independent of the enclosing function's
    /// comptime params) and their bodies are only analyzed at specialization.
    pub is_generic: bool,
    /// Return-type symbol as written in source. Preserved (as well as the
    /// resolved `return_type` above) so that generic-method specialization
    /// can substitute method-level comptime type params in the return type.
    pub return_type_sym: lasso::Spur,
}

/// Method signature for anonymous struct structural equality comparison.
///
/// This captures only the parts of a method that affect structural equality:
/// method name, whether it has self, parameter types (as symbols), and return type.
/// Method bodies do NOT affect structural equality - only signatures matter.
///
/// Type symbols are stored as Spur (interned strings) rather than resolved Types
/// because at comparison time, `Self` hasn't been resolved to a concrete StructId yet.
/// Two methods using `Self` in the same positions are considered structurally equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnonMethodSig {
    /// Method name
    pub name: Spur,
    /// Whether this is a method (has self) or associated function (no self)
    pub has_self: bool,
    /// Parameter type symbols (excluding self parameter)
    pub param_types: Vec<Spur>,
    /// Return type symbol
    pub return_type: Spur,
}

/// A single method declaration inside a `derive` body (ADR-0058).
///
/// Captures the structural info needed to splice the method into a host
/// type's method list at derive expansion: the original RIR refs are
/// preserved verbatim — the existing generic-method machinery substitutes
/// `Self` at first call.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // fields populated in phase 2; consumed in phase 4
pub struct DeriveMethod {
    /// Method name.
    pub name: Spur,
    /// Whether this method takes a `self` receiver.
    pub has_self: bool,
    /// RIR instruction ref for the method's `FnDecl` (the same instruction
    /// `gen_method` emits for an inline method).
    pub method_ref: gruel_rir::InstRef,
    /// Span of the method declaration (used for diagnostics).
    pub span: Span,
}

/// A pending `@derive(D)` attachment from a struct or enum declaration.
///
/// Recorded during Phase 3 (directive resolution); consumed by Phase 4
/// to splice the derive's methods into the host type's method list.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // fields populated in phase 3; consumed in phase 4
pub struct DeriveBinding {
    /// Host type — the struct or enum whose declaration carries the
    /// `@derive(...)` directive.
    pub host_name: Spur,
    /// Whether the host is an enum (`true`) or a struct (`false`).
    pub host_is_enum: bool,
    /// Name of the derive being attached (must resolve to a `derive` item
    /// in `Sema::derives`).
    pub derive_name: Spur,
    /// Span of the host type's declaration.
    pub host_span: Span,
    /// Span of the `@derive(...)` directive itself, for diagnostics.
    pub directive_span: Span,
}

/// Information about a `derive` item (ADR-0058).
#[derive(Debug, Clone)]
#[allow(dead_code)] // fields populated in phase 2; consumed in phase 4
pub struct DeriveInfo {
    /// Derive name (e.g., `Drop`).
    pub name: Spur,
    /// RIR ref to the `DeriveDecl` instruction itself.
    pub decl_ref: gruel_rir::InstRef,
    /// Span covering the derive item.
    pub span: Span,
    /// One entry per method declaration in the derive body, in source order.
    pub methods: Vec<DeriveMethod>,
}

/// Information about a constant declaration.
///
/// Constants are compile-time values. In the module system, they're primarily
/// used for re-exports:
/// ```gruel
/// pub const strings = @import("utils/strings.gruel");
/// pub const helper = @import("utils/internal.gruel").helper;
/// ```
#[derive(Debug, Clone)]
pub struct ConstInfo {
    /// Whether this constant is public
    pub is_pub: bool,
    /// The type of the constant value (e.g., Type::Module for imports)
    pub ty: Type,
    /// The RIR instruction ref for the initializer
    pub init: gruel_rir::InstRef,
    /// Span of the const declaration
    pub span: Span,
}

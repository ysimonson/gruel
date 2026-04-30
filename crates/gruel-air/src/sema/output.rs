//! Output types from semantic analysis.
//!
//! This module contains the final outputs produced by semantic analysis:
//! - [`AnalyzedFunction`] - A single analyzed function with typed IR
//! - [`SemaOutput`] - Complete output from analyzing a program

use crate::inst::{Air, AirParamMode};
use crate::intern_pool::TypeInternPool;
use crate::types::{InterfaceDef, InterfaceId, StructId, Type};
use gruel_util::CompileWarning;
use lasso::Spur;
use rustc_hash::FxHashMap as HashMap;

/// Vtable witnesses keyed by `(struct_id, interface_id)`. The value is the
/// conforming type's method-key list in interface declaration order; codegen
/// looks each one up in the function map to build the vtable global.
pub type InterfaceVtables = HashMap<(StructId, InterfaceId), Vec<(StructId, Spur)>>;

/// Result of analyzing a function.
#[derive(Debug)]
pub struct AnalyzedFunction {
    pub name: String,
    pub air: Air,
    /// Number of local variable slots needed
    pub num_locals: u32,
    /// Number of ABI slots used by parameters.
    /// For scalar types (i32, bool), each parameter uses 1 slot.
    /// For struct types, each field uses 1 slot (flattened ABI).
    pub num_param_slots: u32,
    /// Passing mode for each parameter slot (normal, inout, or borrow).
    /// Length matches num_param_slots - for struct params, all slots share
    /// the same mode as the original parameter.
    pub param_modes: Vec<AirParamMode>,
    /// Type of each parameter ABI slot, parallel to `param_modes`.
    /// Preserved here so backends can declare correct function signatures
    /// even when DCE has removed unused `Param` instructions from the body.
    pub param_slot_types: Vec<Type>,
    /// Whether this function is a destructor (`drop fn`).
    /// Destructors must not auto-drop their `self` parameter, as the
    /// destructor IS the drop logic for that value.
    pub is_destructor: bool,
}

/// Output from semantic analysis.
///
/// Contains all analyzed functions, struct definitions, enum definitions, and any warnings
/// generated during analysis.
#[derive(Debug)]
pub struct SemaOutput {
    /// Analyzed functions with typed IR.
    pub functions: Vec<AnalyzedFunction>,
    /// String literals indexed by their AIR string_const index.
    pub strings: Vec<String>,
    /// Byte-blob literals indexed by their AIR bytes_const index. Currently
    /// populated by `@embed_file`.
    pub bytes: Vec<Vec<u8>>,
    /// Warnings collected during analysis.
    pub warnings: Vec<CompileWarning>,
    /// Type intern pool (contains all types including arrays).
    pub type_pool: TypeInternPool,
    /// Lines of `@dbg` output collected during comptime evaluation.
    pub comptime_dbg_output: Vec<String>,
    /// Interface definitions (ADR-0056), indexed by `InterfaceId.0`. Codegen
    /// uses these to know each interface's method count and slot order when
    /// emitting vtables.
    pub interface_defs: Vec<InterfaceDef>,
    /// Vtable witnesses keyed by `(struct_id, interface_id)`. The witness is
    /// the conforming type's method-key list in interface declaration order;
    /// codegen looks each one up in the function map to build the vtable
    /// global.
    pub interface_vtables: InterfaceVtables,
}

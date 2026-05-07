//! Control Flow Graph IR for the Gruel compiler.
//!
//! This crate provides a CFG-based intermediate representation that sits
//! between AIR (typed, structured) and X86Mir (machine-specific).
//!
//! The CFG representation makes control flow explicit through basic blocks
//! and terminators, which is essential for:
//! - Linear type checking
//! - Drop elaboration
//! - Liveness analysis
//! - Other dataflow analyses
//!
//! ## Pipeline
//!
//! ```text
//! AIR (structured) → CFG (explicit control flow) → X86Mir (machine code)
//! ```

mod build;
pub mod drop_names;
mod inst;

use gruel_util::CompileWarning;

pub use build::CfgBuilder;
pub use inst::{
    BasicBlock, BlockId, Cfg, CfgArgMode, CfgCallArg, CfgInst, CfgInstData, CfgValue,
    MakeSliceData, Place, PlaceBase, Projection, Terminator,
};

// Re-export types from gruel-air that we use
pub use gruel_air::{StructDef, StructId, Type, TypeKind};

/// Output from CFG construction.
///
/// Contains the constructed CFG along with any warnings detected during
/// construction (e.g., unreachable code).
pub struct CfgOutput {
    /// The constructed control flow graph.
    pub cfg: Cfg,
    /// Warnings detected during CFG construction.
    pub warnings: Vec<CompileWarning>,
}

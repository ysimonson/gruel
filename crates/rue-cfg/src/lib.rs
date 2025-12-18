//! Control Flow Graph IR for the Rue compiler.
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

mod inst;
mod build;

pub use inst::{
    BlockId, Cfg, BasicBlock, CfgInst, CfgInstData, CfgValue, Terminator,
};
pub use build::CfgBuilder;

// Re-export types from rue-air that we use
pub use rue_air::{StructDef, StructId, Type};

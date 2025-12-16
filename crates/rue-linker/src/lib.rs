//! Minimal linker for the Rue compiler.
//!
//! This linker handles:
//! - Reading ELF64 relocatable object files (.o)
//! - Creating ELF64 relocatable object files
//! - Resolving symbols between objects
//! - Applying relocations
//! - Producing a final executable
//!
//! It's intentionally minimal - just enough to link Rue code with its runtime.

mod elf;
mod emit;
mod linker;

pub use elf::{ObjectFile, Section, Symbol, Relocation, RelocationType};
pub use emit::{ObjectBuilder, CodeRelocation};
pub use linker::{Linker, LinkError};

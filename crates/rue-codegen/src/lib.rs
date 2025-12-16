//! Code generation for the Rue compiler.
//!
//! Converts AIR to x86-64 machine code.

mod x86_64;

pub use x86_64::CodeGen;

//! Shared utilities for the Gruel compiler:
//!
//! - [`span`] — source location tracking
//! - [`error`] — compile-error / diagnostic types
//! - [`ice`] — internal-compiler-error context capture
//! - [`ops`] — `BinOp` / `UnaryOp` shared by RIR, AIR, and CFG
//!
//! The most commonly used items are re-exported at the crate root so that
//! `use gruel_util::{Span, CompileError, BinOp};` works without reaching
//! into module paths.

pub mod error;
pub mod ice;
pub mod ops;
pub mod place;
pub mod span;

pub use error::*;
pub use ops::{BinOp, UnaryOp};
pub use place::PlaceBase;
pub use span::{FileId, Span};

//! Language Server Protocol implementation for Gruel (ADR-0091).
//!
//! This crate is the engine behind the `gruel lsp` subcommand. It depends on
//! `gruel-compiler` and reuses the existing frontend pipeline; what we add
//! on top is the LSP message pump (`tower-lsp`), incremental document state,
//! and the mapping from Gruel `JsonDiagnostic` values to LSP types.

pub mod analysis;
pub mod code_actions;
pub mod diagnostics;
pub mod document;
pub mod position;
pub mod server;
pub mod workspace;

pub use server::{Backend, run_server, run_stdio_server};

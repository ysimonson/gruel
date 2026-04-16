//! Gruel Runtime Library
//!
//! This crate provides minimal runtime support for Gruel programs.
//! It's designed to be compiled as a staticlib and linked into
//! Gruel executables.
//!
//! # Overview
//!
//! The Gruel compiler generates machine code that calls into this runtime
//! for certain operations that can't be efficiently or safely inlined:
//!
//! - **Process exit**: When `main()` returns, generated code calls [`__gruel_exit`](entry::__gruel_exit)
//!   with the return value as the exit code.
//! - **Runtime errors**: Division by zero and integer overflow trigger calls to
//!   error handlers in the [`error`] module.
//! - **Debug output**: The `@dbg` builtin calls functions in the [`debug`] module.
//! - **String operations**: String equality, allocation, and methods are in the [`string`] module.
//! - **I/O operations**: Input functions like `readLine()` are in the [`io`] module.
//!
//! # Platform Requirements
//!
//! This runtime supports the following platforms:
//!
//! - **x86-64 Linux**
//! - **aarch64 Linux**
//! - **aarch64 macOS**
//!
//! Platform-specific operations are handled via libc.
//!
//! # Design Philosophy
//!
//! The runtime is deliberately minimal:
//!
//! - **`#![no_std]`**: No dependency on the Rust standard library.
//!   OS interaction happens via libc.
//! - **Zero allocations**: The runtime never allocates memory (except when explicitly
//!   requested via `__gruel_alloc`).
//! - **Small code size**: Compiled with `-Copt-level=z` and LTO for minimal footprint.
//!
//! # Calling Conventions
//!
//! All public functions use the C ABI (`extern "C"`) and are `#[no_mangle]` so
//! they can be called from Gruel-generated machine code. The compiler generates
//! `call` instructions to these symbol names.
//!
//! # Exit Codes
//!
//! Gruel programs use the following exit codes by convention:
//!
//! | Code | Meaning |
//! |------|---------|
//! | 0 | Success (or whatever `main()` returned) |
//! | 1 | Panic (from Rust runtime, shouldn't happen in normal operation) |
//! | 101 | Runtime error (division by zero, overflow) |
//!
//! # Integration with the Compiler
//!
//! The `gruel-compiler` crate links this runtime library into every Gruel executable.
//! The runtime is compiled as a static library (`.a` file) and its symbols are
//! referenced by generated code in `gruel-codegen-llvm`.

#![no_std]

// Compile error for unsupported platforms
#[cfg(not(any(
    all(target_arch = "x86_64", target_os = "linux"),
    all(target_arch = "aarch64", target_os = "macos"),
    all(target_arch = "aarch64", target_os = "linux")
)))]
compile_error!(
    "gruel-runtime only supports x86-64 Linux, aarch64 Linux, and aarch64 macOS. \
     Other platforms are not currently supported."
);

// Platform abstraction layer (backed by libc)
pub mod platform;

// Heap allocation
mod heap;

// Runtime modules
pub mod debug;
pub mod entry;
pub mod error;
pub mod io;
pub mod parse;
pub mod random;
pub mod string;

// Re-export StringResult for use by other modules
pub use string::StringResult;

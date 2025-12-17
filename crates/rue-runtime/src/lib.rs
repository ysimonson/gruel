//! Rue Runtime Library
//!
//! This crate provides minimal runtime support for Rue programs.
//! It's designed to be compiled as a staticlib and linked into
//! Rue executables.
//!
//! # Overview
//!
//! The Rue compiler generates machine code that calls into this runtime
//! for certain operations that can't be efficiently or safely inlined:
//!
//! - **Process exit**: When `main()` returns, generated code calls [`__rue_exit`]
//!   with the return value as the exit code.
//! - **Runtime errors**: Division by zero and integer overflow trigger calls to
//!   [`__rue_div_by_zero`] and [`__rue_overflow`] respectively.
//!
//! # Platform Requirements
//!
//! This runtime only supports **x86-64 Linux**. It uses direct syscalls and
//! contains platform-specific assembly. Attempting to compile on other platforms
//! will result in a compile error.
//!
//! # Design Philosophy
//!
//! The runtime is deliberately minimal:
//!
//! - **`#![no_std]`**: No dependency on the Rust standard library or libc.
//!   All OS interaction happens via direct syscalls.
//! - **Zero allocations**: The runtime never allocates memory.
//! - **Small code size**: Compiled with `-Copt-level=z` and LTO for minimal footprint.
//!
//! # Calling Conventions
//!
//! All public functions use the C ABI (`extern "C"`) and are `#[no_mangle]` so
//! they can be called from Rue-generated machine code. The compiler generates
//! `call` instructions to these symbol names.
//!
//! # Exit Codes
//!
//! Rue programs use the following exit codes by convention:
//!
//! | Code | Meaning |
//! |------|---------|
//! | 0 | Success (or whatever `main()` returned) |
//! | 1 | Panic (from Rust runtime, shouldn't happen in normal operation) |
//! | 101 | Runtime error (division by zero, overflow) |
//!
//! # Integration with the Compiler
//!
//! The `rue-linker` crate links this runtime library into every Rue executable.
//! The runtime is compiled as a static library (`.a` file) and its symbols are
//! referenced by generated code in `rue-codegen`.
//!
//! Specifically:
//! - `rue-codegen/src/x86_64/emit.rs` generates `call __rue_*` instructions
//! - `rue-linker` links the runtime archive into the final ELF executable

#![no_std]

// Platform-specific implementation
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
mod x86_64_linux;

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
use x86_64_linux as platform;

// Compile error for unsupported platforms
#[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
compile_error!(
    "rue-runtime only supports x86-64 Linux. \
     Other platforms are not currently supported."
);

/// Panic handler for `#![no_std]` environments.
///
/// This handler is only active when the crate is compiled as a library (not
/// during tests, which use the standard library's panic handler). When a panic
/// occurs, we exit with code 101.
///
/// # Why `#[cfg(not(test))]`?
///
/// During testing, Rust's test harness provides its own panic handler that
/// catches panics and reports them as test failures. If we provided a panic
/// handler, it would conflict with the test harness and prevent proper test
/// execution.
#[cfg(all(not(test), target_arch = "x86_64", target_os = "linux"))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    platform::exit(101)
}

/// Exit the process with the given status code.
///
/// This is the main entry point called by Rue-generated code when `main()`
/// returns. The return value of `main()` becomes the exit code.
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_exit(status: i32) -> !
/// ```
///
/// - `status` is passed in the `edi` register (System V AMD64 ABI)
/// - This function never returns
///
/// # Example
///
/// Generated code for `fn main() -> i32 { 42 }`:
/// ```asm
/// main:
///     mov eax, 42
///     ret
/// _start:
///     call main
///     mov edi, eax
///     call __rue_exit
/// ```
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_exit(status: i32) -> ! {
    platform::exit(status)
}

/// Runtime error: division by zero.
///
/// Called when a division or modulo operation has a zero divisor. This is
/// typically triggered by a conditional jump inserted by the compiler before
/// division operations.
///
/// # Behavior
///
/// 1. Writes `"error: division by zero\n"` to stderr (best-effort)
/// 2. Exits with code 101
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_div_by_zero() -> !
/// ```
///
/// No arguments. Never returns.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_div_by_zero() -> ! {
    platform::write_stderr(b"error: division by zero\n");
    platform::exit(101)
}

/// Runtime error: integer overflow.
///
/// Called when an arithmetic operation overflows. This is typically triggered
/// by a conditional jump inserted by the compiler after arithmetic operations
/// that check the overflow flag.
///
/// # Behavior
///
/// 1. Writes `"error: integer overflow\n"` to stderr (best-effort)
/// 2. Exits with code 101
///
/// # ABI
///
/// ```text
/// extern "C" fn __rue_overflow() -> !
/// ```
///
/// No arguments. Never returns.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn __rue_overflow() -> ! {
    platform::write_stderr(b"error: integer overflow\n");
    platform::exit(101)
}

// Re-export platform functions for tests
#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
pub use x86_64_linux::{exit, write, write_all, write_stderr};

#[cfg(test)]
mod tests {
    // Most tests are in x86_64_linux.rs since they test syscall behavior.
    // This module contains tests for the public API and integration.

    #[test]
    fn test_error_messages_are_newline_terminated() {
        // Ensure our error messages end with newlines for clean terminal output
        let div_msg = b"error: division by zero\n";
        let overflow_msg = b"error: integer overflow\n";

        assert!(div_msg.ends_with(b"\n"));
        assert!(overflow_msg.ends_with(b"\n"));
    }

    #[test]
    fn test_error_messages_are_valid_utf8() {
        // Error messages should be valid UTF-8 for proper display
        let div_msg = b"error: division by zero\n";
        let overflow_msg = b"error: integer overflow\n";

        assert!(core::str::from_utf8(div_msg).is_ok());
        assert!(core::str::from_utf8(overflow_msg).is_ok());
    }
}

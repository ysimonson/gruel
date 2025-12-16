//! Rue Runtime Library
//!
//! This crate provides minimal runtime support for Rue programs.
//! It's designed to be compiled as a staticlib and linked into
//! Rue executables.
//!
//! The runtime is `#![no_std]` to avoid libc dependencies and
//! uses direct syscalls for all OS interaction.

#![no_std]

mod x86_64_linux;

/// Panic handler for no_std.
///
/// In case of panic, we exit with error code 101 (similar to Rust's
/// convention for panics).
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    x86_64_linux::exit(101)
}

/// Exit the process with the given status code.
///
/// This is the main entry point called by Rue-generated code
/// when `main()` returns. The return value of main becomes
/// the exit code.
///
/// # Safety
///
/// This function is marked `extern "C"` and `#[no_mangle]` so it
/// can be called from Rue-generated machine code.
#[unsafe(no_mangle)]
pub extern "C" fn __rue_exit(status: i32) -> ! {
    x86_64_linux::exit(status)
}

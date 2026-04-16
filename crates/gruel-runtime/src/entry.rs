//! Program entry points and exit handling.
//!
//! This module provides:
//! - Platform-specific `_start` / `_main` entry points
//! - `__gruel_exit` function called when main() returns
//! - Panic handler for no_std environments

use crate::platform;

/// Panic handler for `#![no_std]` environments.
#[cfg(all(
    not(test),
    any(
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "aarch64", target_os = "linux")
    )
))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    platform::exit(101)
}

/// Program entry point for Linux x86-64.
#[cfg(all(not(test), target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    use core::arch::asm;

    unsafe extern "C" {
        fn main() -> i32;
    }

    let exit_code: i32;
    unsafe {
        asm!(
            "sub rsp, 8",
            "call {main}",
            "mov edi, eax",
            main = sym main,
            out("edi") exit_code,
            clobber_abi("C"),
        );
    }
    platform::exit(exit_code)
}

/// Program entry point for macOS aarch64.
#[cfg(all(not(test), target_arch = "aarch64", target_os = "macos"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _main() -> ! {
    use core::arch::asm;

    unsafe extern "C" {
        fn main() -> i32;
    }

    let exit_code: i32;
    unsafe {
        asm!(
            "bl {main}",
            main = sym main,
            lateout("w0") exit_code,
            clobber_abi("C"),
        );
    }
    platform::exit(exit_code)
}

/// Program entry point for Linux aarch64.
#[cfg(all(not(test), target_arch = "aarch64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    use core::arch::asm;

    unsafe extern "C" {
        fn main() -> i32;
    }

    let exit_code: i32;
    unsafe {
        asm!(
            "bl {main}",
            main = sym main,
            lateout("w0") exit_code,
            clobber_abi("C"),
        );
    }
    platform::exit(exit_code)
}

/// Exit the process with the given status code.
///
/// Called by Gruel-generated code when `main()` returns.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_exit(status: i32) -> ! {
    platform::exit(status)
}

#[cfg(test)]
mod tests {
    // Entry point tests require process spawning; covered via spec tests.
}

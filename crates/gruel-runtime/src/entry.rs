//! Program entry points and exit handling.
//!
//! ADR-0087 Phase 4 retired the `__gruel_exit` shim — main-return
//! codegen now emits a direct call to libc `exit` (declared with a
//! `noreturn` LLVM attribute the same way `__gruel_exit` was). This
//! module still hosts the platform-specific `_start` / `_main`
//! entry points and the `#![no_std]` panic handler.

#[cfg(not(test))]
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

#[cfg(test)]
mod tests {
    // Entry point tests require process spawning; covered via spec tests.
}

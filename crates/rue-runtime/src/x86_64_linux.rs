//! x86-64 Linux syscall implementations.
//!
//! This module provides direct syscall wrappers for Linux on x86-64.
//! No libc is used - we invoke the kernel directly.

use core::arch::asm;

/// Linux syscall number for write.
const SYS_WRITE: i64 = 1;

/// Linux syscall number for exit.
const SYS_EXIT: i64 = 60;

/// Standard error file descriptor.
const STDERR: i64 = 2;

/// Write bytes to a file descriptor.
///
/// Returns the number of bytes written, or a negative error code.
pub fn write(fd: i64, buf: *const u8, len: usize) -> i64 {
    let result: i64;
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_WRITE,
            in("rdi") fd,
            in("rsi") buf,
            in("rdx") len,
            lateout("rax") result,
            // syscall clobbers rcx and r11
            out("rcx") _,
            out("r11") _,
        );
    }
    result
}

/// Write a message to stderr.
pub fn write_stderr(msg: &[u8]) {
    write(STDERR, msg.as_ptr(), msg.len());
}

/// Exit the process with the given status code.
///
/// This performs a direct syscall to `exit(2)` and never returns.
pub fn exit(status: i32) -> ! {
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_EXIT,
            in("rdi") status as i64,
            options(noreturn)
        );
    }
}

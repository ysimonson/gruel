//! x86-64 Linux syscall implementations.
//!
//! This module provides direct syscall wrappers for Linux on x86-64.
//! No libc is used - we invoke the kernel directly via the `syscall` instruction.
//!
//! # Platform Requirements
//!
//! This module only compiles on x86-64 Linux. Attempting to compile on other
//! platforms will result in a compile error.
//!
//! # Syscall Conventions
//!
//! On x86-64 Linux:
//! - Syscall number goes in `rax`
//! - Arguments go in `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9` (in order)
//! - Return value comes back in `rax`
//! - `rcx` and `r11` are clobbered by the syscall instruction
//!
//! # Error Handling
//!
//! Linux syscalls return negative values on error (specifically, `-errno`).
//! This module preserves those error codes in return values where applicable.

// Compile-time check for platform requirements
#[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
compile_error!("rue-runtime only supports x86-64 Linux");

use core::arch::asm;

/// Linux syscall number for write (see `man 2 write`).
const SYS_WRITE: i64 = 1;

/// Linux syscall number for exit (see `man 2 exit`).
const SYS_EXIT: i64 = 60;

/// Standard error file descriptor.
const STDERR: i64 = 2;

/// Write bytes to a file descriptor.
///
/// This is a thin wrapper around the Linux `write(2)` syscall.
///
/// # Arguments
///
/// * `fd` - File descriptor to write to
/// * `buf` - Pointer to the buffer containing data to write
/// * `len` - Number of bytes to write
///
/// # Returns
///
/// On success, returns the number of bytes written (which may be less than `len`
/// if the write was interrupted or the pipe/socket buffer is full).
///
/// On error, returns a negative value representing `-errno`.
///
/// # Safety
///
/// The caller must ensure:
/// - `buf` points to a valid memory region of at least `len` bytes
/// - The memory region remains valid for the duration of the syscall
/// - `buf` is properly aligned (though byte alignment is always satisfied)
///
/// # Note on Partial Writes
///
/// This function may write fewer bytes than requested. Callers should check
/// the return value and use [`write_all`] if complete writes are required.
pub fn write(fd: i64, buf: *const u8, len: usize) -> i64 {
    let result: i64;
    // SAFETY: We're making a syscall with the provided arguments.
    // The caller is responsible for ensuring buf/len are valid.
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_WRITE,
            in("rdi") fd,
            in("rsi") buf,
            in("rdx") len,
            lateout("rax") result,
            // syscall clobbers rcx and r11 per x86-64 ABI
            out("rcx") _,
            out("r11") _,
        );
    }
    result
}

/// Write all bytes to a file descriptor, handling partial writes.
///
/// This function loops until all bytes are written or an unrecoverable error occurs.
/// It handles partial writes by advancing the buffer pointer and retrying.
///
/// # Arguments
///
/// * `fd` - File descriptor to write to
/// * `buf` - Slice of bytes to write
///
/// # Returns
///
/// * `Ok(())` - All bytes were successfully written
/// * `Err(errno)` - A syscall error occurred (errno is positive)
///
/// # Behavior on Error
///
/// If a write fails (returns negative), this function returns immediately with
/// the error code. No retry is attempted on error.
///
/// # Safety
///
/// This function is safe because it only accepts a slice, which guarantees
/// valid memory.
pub fn write_all(fd: i64, mut buf: &[u8]) -> Result<(), i64> {
    while !buf.is_empty() {
        let result = write(fd, buf.as_ptr(), buf.len());
        if result < 0 {
            // Syscall error - return the errno (as positive)
            return Err(-result);
        }
        if result == 0 {
            // This shouldn't happen for stderr, but handle it to avoid infinite loop.
            // A zero return typically means the fd is in a state where no more data
            // can be written (e.g., closed pipe). We treat this as an error.
            return Err(5); // EIO - I/O error
        }
        // Advance past the bytes we successfully wrote
        buf = &buf[result as usize..];
    }
    Ok(())
}

/// Write a message to stderr.
///
/// This is a best-effort write operation. If writing fails (e.g., stderr is
/// closed or redirected to a broken pipe), the error is silently ignored
/// because there's no meaningful recovery action - the runtime is typically
/// about to exit anyway.
///
/// This function handles partial writes by looping until all bytes are written
/// or an error occurs.
///
/// # Arguments
///
/// * `msg` - The bytes to write to stderr
pub fn write_stderr(msg: &[u8]) {
    // Best-effort: ignore errors since we're typically about to exit
    // and there's no way to report the error anyway
    let _ = write_all(STDERR, msg);
}

/// Exit the process with the given status code.
///
/// This performs a direct syscall to `exit(2)` and never returns.
/// The process terminates immediately without running any cleanup code
/// or destructors.
///
/// # Arguments
///
/// * `status` - The exit code to return to the parent process.
///   By convention: 0 for success, non-zero for failure.
///   Rue uses: 0 for success, 1 for panic, 101 for runtime errors.
///
/// # Exit Codes
///
/// This function never returns - it terminates the process.
pub fn exit(status: i32) -> ! {
    // SAFETY: The exit syscall is always safe to call and never returns.
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_EXIT,
            in("rdi") status as i64,
            options(noreturn)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: We can't easily test exit() or the functions that call it
    // because they terminate the process. Those are tested via integration
    // tests that spawn child processes.

    #[test]
    fn test_write_to_stderr() {
        // Test that write() returns the correct byte count
        let msg = b"test message\n";
        let result = write(STDERR, msg.as_ptr(), msg.len());
        assert_eq!(result, msg.len() as i64);
    }

    #[test]
    fn test_write_empty() {
        // Writing zero bytes should succeed and return 0
        let result = write(STDERR, core::ptr::null(), 0);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_write_invalid_fd() {
        // Writing to an invalid fd should return an error
        let msg = b"test";
        let result = write(999, msg.as_ptr(), msg.len());
        // Should return -EBADF (9) for bad file descriptor
        assert!(result < 0);
        assert_eq!(-result, 9); // EBADF
    }

    #[test]
    fn test_write_all_success() {
        let msg = b"write_all test\n";
        let result = write_all(STDERR, msg);
        assert!(result.is_ok());
    }

    #[test]
    fn test_write_all_empty() {
        // Writing empty slice should succeed immediately
        let result = write_all(STDERR, b"");
        assert!(result.is_ok());
    }

    #[test]
    fn test_write_all_invalid_fd() {
        let msg = b"test";
        let result = write_all(999, msg);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), 9); // EBADF
    }

    #[test]
    fn test_write_stderr_helper() {
        // This should not panic, even with various inputs
        write_stderr(b"stderr test 1\n");
        write_stderr(b"");
        write_stderr(b"stderr test 2\n");
    }

    #[test]
    fn test_write_large_message() {
        // Test writing a larger message to exercise any partial write handling
        // Use a stack-allocated array since we're no_std
        let msg = [b'x'; 4096];
        let result = write(STDERR, msg.as_ptr(), msg.len());
        // Should write all bytes (stderr is typically unbuffered)
        assert_eq!(result, 4096);
    }

    #[test]
    fn test_syscall_constants() {
        // Verify our syscall numbers match Linux x86-64 ABI
        assert_eq!(SYS_WRITE, 1);
        assert_eq!(SYS_EXIT, 60);
        assert_eq!(STDERR, 2);
    }
}

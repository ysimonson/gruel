//! Platform abstraction layer.
//!
//! Provides I/O, memory mapping, process control, and printing primitives.
//! Functions are declared as extern "C" and resolved from libc at link time
//! (the compiler links with `-nostartfiles` rather than `-nostdlib`).

// =============================================================================
// libc FFI declarations
// =============================================================================

// We declare just the libc functions we need rather than depending on the libc
// crate, because the runtime is compiled directly by rustc (not through Cargo).

unsafe extern "C" {
    fn write(fd: i32, buf: *const u8, count: usize) -> isize;
    fn read(fd: i32, buf: *mut u8, count: usize) -> isize;
    fn mmap(addr: *mut u8, length: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
    fn munmap(addr: *mut u8, length: usize) -> i32;
    fn _exit(status: i32) -> !;
    fn dprintf(fd: i32, fmt: *const u8, ...) -> i32;
    pub(crate) fn malloc(size: usize) -> *mut u8;
    pub(crate) fn realloc(ptr: *mut u8, size: usize) -> *mut u8;
    pub(crate) fn free(ptr: *mut u8);
}

// ADR-0087 Phase 3: the runtime's `__gruel_read_line` (which used
// libc `getline` + the `stdin` FILE*) is gone — the prelude
// `read_line()` fn drives libc `read(0, …)` directly, so the
// `File` / `getline` / `stdin` extern declarations and platform
// symbol-name shim no longer have a caller in the runtime.

#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn getrandom(buf: *mut u8, buflen: usize, flags: u32) -> isize;
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn getentropy(buf: *mut u8, buflen: usize) -> i32;
}

// mmap constants (same on Linux and macOS)
const PROT_READ: i32 = 0x1;
const PROT_WRITE: i32 = 0x2;
const MAP_PRIVATE: i32 = 0x02;

#[cfg(target_os = "linux")]
const MAP_ANONYMOUS: i32 = 0x20;

#[cfg(target_os = "macos")]
const MAP_ANONYMOUS: i32 = 0x1000;

/// Sentinel returned by mmap on failure.
const MAP_FAILED: *mut u8 = !0usize as *mut u8;

// =============================================================================
// Public API
// =============================================================================

/// Standard input file descriptor.
pub const STDIN: u64 = 0;

/// Standard output file descriptor.
pub const STDOUT: u64 = 1;

/// Standard error file descriptor.
pub const STDERR: u64 = 2;

/// Write bytes to a file descriptor.
pub fn sys_write(fd: u64, buf: *const u8, len: usize) -> i64 {
    unsafe { write(fd as i32, buf, len) as i64 }
}

/// Read bytes from a file descriptor.
pub fn sys_read(fd: u64, buf: *mut u8, len: usize) -> i64 {
    unsafe { read(fd as i32, buf, len) as i64 }
}

/// Write all bytes to a file descriptor, handling partial writes.
pub fn write_all(fd: u64, mut buf: &[u8]) -> Result<(), i64> {
    while !buf.is_empty() {
        let result = sys_write(fd, buf.as_ptr(), buf.len());
        if result < 0 {
            return Err(-result);
        }
        if result == 0 {
            return Err(5); // EIO
        }
        buf = &buf[result as usize..];
    }
    Ok(())
}

/// Write a message to stderr (best-effort).
pub fn write_stderr(msg: &[u8]) {
    let _ = write_all(STDERR, msg);
}

/// Write a message to stdout (best-effort).
pub fn write_stdout(msg: &[u8]) {
    let _ = write_all(STDOUT, msg);
}

/// Map anonymous memory pages. Returns null on failure.
pub fn sys_mmap(size: usize) -> *mut u8 {
    let ptr = unsafe {
        mmap(
            core::ptr::null_mut(),
            size,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if ptr == MAP_FAILED {
        core::ptr::null_mut()
    } else {
        ptr
    }
}

/// Unmap memory pages previously mapped with `sys_mmap`.
pub fn sys_munmap(addr: *mut u8, size: usize) -> i64 {
    unsafe { munmap(addr, size) as i64 }
}

/// Exit the process immediately.
pub fn exit(status: i32) -> ! {
    unsafe { _exit(status) }
}

/// Fill a buffer with random bytes from the OS entropy source.
pub fn get_random_bytes(buf: &mut [u8]) {
    #[cfg(target_os = "linux")]
    {
        let result = unsafe { getrandom(buf.as_mut_ptr(), buf.len(), 0) };
        if result < 0 || result as usize != buf.len() {
            write_stderr(b"error: random number generation failed\n");
            exit(101);
        }
    }

    #[cfg(target_os = "macos")]
    {
        let result = unsafe { getentropy(buf.as_mut_ptr(), buf.len()) };
        if result != 0 {
            write_stderr(b"error: random number generation failed\n");
            exit(101);
        }
    }
}

/// Convert a signed 64-bit integer to decimal and write to stdout.
pub fn print_i64(value: i64) {
    // %lld\n\0
    unsafe { dprintf(STDOUT as i32, b"%lld\n\0".as_ptr(), value) };
}

/// Convert an unsigned 64-bit integer to decimal and write to stdout.
pub fn print_u64(value: u64) {
    // %llu\n\0
    unsafe { dprintf(STDOUT as i32, b"%llu\n\0".as_ptr(), value) };
}

/// Print a boolean value to stdout.
pub fn print_bool(value: bool) {
    if value {
        write_stdout(b"true\n");
    } else {
        write_stdout(b"false\n");
    }
}

/// Convert a signed 64-bit integer to decimal and write to stdout without a trailing newline.
pub fn print_i64_noln(value: i64) {
    // %lld\0
    unsafe { dprintf(STDOUT as i32, b"%lld\0".as_ptr(), value) };
}

/// Convert an unsigned 64-bit integer to decimal and write to stdout without a trailing newline.
pub fn print_u64_noln(value: u64) {
    // %llu\0
    unsafe { dprintf(STDOUT as i32, b"%llu\0".as_ptr(), value) };
}

/// Print a boolean value to stdout without a trailing newline.
pub fn print_bool_noln(value: bool) {
    if value {
        write_stdout(b"true");
    } else {
        write_stdout(b"false");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_to_stderr() {
        let msg = b"test message\n";
        let result = sys_write(STDERR, msg.as_ptr(), msg.len());
        assert_eq!(result, msg.len() as i64);
    }

    #[test]
    fn test_write_empty() {
        let result = sys_write(STDERR, core::ptr::null(), 0);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_write_invalid_fd() {
        let msg = b"test";
        let result = sys_write(999, msg.as_ptr(), msg.len());
        assert!(result < 0);
    }

    #[test]
    fn test_write_all_success() {
        let msg = b"write_all test\n";
        let result = write_all(STDERR, msg);
        assert!(result.is_ok());
    }

    #[test]
    fn test_write_all_empty() {
        let result = write_all(STDERR, b"");
        assert!(result.is_ok());
    }

    #[test]
    fn test_write_all_invalid_fd() {
        let msg = b"test";
        let result = write_all(999, msg);
        assert!(result.is_err());
    }

    #[test]
    fn test_write_stderr_helper() {
        write_stderr(b"stderr test 1\n");
        write_stderr(b"");
        write_stderr(b"stderr test 2\n");
    }

    #[test]
    fn test_mmap_basic() {
        let size = 4096;
        let ptr = sys_mmap(size);
        assert!(!ptr.is_null());

        unsafe {
            assert_eq!(*ptr, 0);
            *ptr = 42;
            assert_eq!(*ptr, 42);
        }

        let result = sys_munmap(ptr, size);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_mmap_large() {
        let size = 1024 * 1024;
        let ptr = sys_mmap(size);
        assert!(!ptr.is_null());

        unsafe {
            *ptr = 1;
            *ptr.add(size - 1) = 2;
            assert_eq!(*ptr, 1);
            assert_eq!(*ptr.add(size - 1), 2);
        }

        let result = sys_munmap(ptr, size);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_mmap_multiple() {
        let size = 4096;
        let ptr1 = sys_mmap(size);
        let ptr2 = sys_mmap(size);
        let ptr3 = sys_mmap(size);

        assert!(!ptr1.is_null());
        assert!(!ptr2.is_null());
        assert!(!ptr3.is_null());
        assert_ne!(ptr1, ptr2);
        assert_ne!(ptr2, ptr3);
        assert_ne!(ptr1, ptr3);

        assert_eq!(sys_munmap(ptr1, size), 0);
        assert_eq!(sys_munmap(ptr2, size), 0);
        assert_eq!(sys_munmap(ptr3, size), 0);
    }

    #[test]
    fn test_mmap_zero_size() {
        let ptr = sys_mmap(0);
        assert!(ptr.is_null());
    }

    #[test]
    fn test_read_invalid_fd() {
        let mut buf = [0u8; 16];
        let result = sys_read(999, buf.as_mut_ptr(), buf.len());
        assert!(result < 0);
    }

    #[test]
    fn test_read_zero_bytes() {
        let mut buf = [0u8; 16];
        let result = sys_read(STDIN, buf.as_mut_ptr(), 0);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_get_random_bytes() {
        let mut buf = [0u8; 16];
        get_random_bytes(&mut buf);
        let all_zeros = buf.iter().all(|&b| b == 0);
        assert!(!all_zeros, "get_random_bytes returned all zeros");
    }
}

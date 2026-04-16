//! Input/Output functions.
//!
//! This module provides I/O operations for Gruel programs:
//! - `__gruel_read_line` - Read a line from standard input

use crate::platform;
use crate::string::StringResult;

/// Read a line from standard input.
///
/// Reads bytes from stdin until a newline (`\n`) or EOF. Returns the line
/// as a String (excluding the trailing newline) via sret convention.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn __gruel_read_line(out: *mut StringResult) {
    let mut ptr: *mut u8 = core::ptr::null_mut();
    let mut buf_size: usize = 0;

    let nread = unsafe { platform::getline(&mut ptr, &mut buf_size, platform::stdin) };

    if nread < 0 {
        // EOF or error with no data
        unsafe { platform::free(ptr) };
        platform::write_stderr(b"error: unexpected end of input\n");
        platform::exit(101);
    }

    let mut len = nread as u64;

    // Strip trailing newline if present
    if len > 0 && unsafe { *ptr.add(len as usize - 1) } == b'\n' {
        len -= 1;
    }

    // SAFETY: Caller provides valid sret pointer.
    unsafe {
        (*out).ptr = ptr;
        (*out).len = len;
        (*out).cap = buf_size as u64;
    }
}

//! Debug intrinsics for the `@dbg` builtin.
//!
//! These functions are called by generated code when `@dbg(expr)` is used.
//! Each type has its own debug function that prints the value followed by
//! a newline to stdout.

use crate::platform;

/// Debug intrinsic: print a signed 64-bit integer.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_dbg_i64(value: i64) {
    platform::print_i64(value);
}

/// Debug intrinsic: print an unsigned 64-bit integer.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_dbg_u64(value: u64) {
    platform::print_u64(value);
}

/// Debug intrinsic: print a boolean.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_dbg_bool(value: i64) {
    platform::print_bool(value != 0);
}

/// Debug intrinsic: print a string.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_dbg_str(ptr: *const u8, len: u64) {
    // SAFETY: Caller guarantees ptr points to valid UTF-8 of len bytes.
    let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
    platform::write_stdout(bytes);
    platform::write_stdout(b"\n");
}

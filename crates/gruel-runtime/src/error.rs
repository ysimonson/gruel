//! Runtime error handlers.
//!
//! These functions are called by generated code when runtime errors occur:
//! - Division by zero
//! - Integer overflow
//! - Integer cast overflow
//! - Array bounds check failure
//!
//! All errors exit with code 101 after writing an error message to stderr.

use crate::platform;

/// Runtime error: division by zero.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_div_by_zero() -> ! {
    platform::write_stderr(b"error: division by zero\n");
    platform::exit(101)
}

/// Runtime error: integer overflow.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_overflow() -> ! {
    platform::write_stderr(b"error: integer overflow\n");
    platform::exit(101)
}

/// Runtime error: integer cast overflow.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_intcast_overflow() -> ! {
    platform::write_stderr(b"error: integer cast overflow\n");
    platform::exit(101)
}

/// Runtime error: index out of bounds.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_bounds_check() -> ! {
    platform::write_stderr(b"error: index out of bounds\n");
    platform::exit(101)
}

/// Runtime error: float-to-integer cast overflow (value is NaN or out of range).
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_float_to_int_overflow() -> ! {
    platform::write_stderr(b"error: float-to-integer cast overflow\n");
    platform::exit(101)
}

/// User-triggered panic with a message.
///
/// Called by `@panic("message")` after the message string has been
/// extracted to a (ptr, len) pair. Writes "panic: <message>\n" to stderr
/// and exits with code 101.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_panic(msg_ptr: *const u8, msg_len: u64) -> ! {
    platform::write_stderr(b"panic: ");
    if !msg_ptr.is_null() && msg_len > 0 {
        let slice = unsafe { core::slice::from_raw_parts(msg_ptr, msg_len as usize) };
        platform::write_stderr(slice);
    }
    platform::write_stderr(b"\n");
    platform::exit(101)
}

/// User-triggered panic with no message.
///
/// Called by `@panic()`. Writes "panic\n" to stderr and exits with code 101.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_panic_no_msg() -> ! {
    platform::write_stderr(b"panic\n");
    platform::exit(101)
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_error_message_lengths() {
        assert_eq!(b"error: division by zero\n".len(), 24);
        assert_eq!(b"error: integer overflow\n".len(), 24);
        assert_eq!(b"error: integer cast overflow\n".len(), 29);
        assert_eq!(b"error: index out of bounds\n".len(), 27);
        assert_eq!(b"error: float-to-integer cast overflow\n".len(), 38);
    }

    #[test]
    fn test_error_messages_are_valid_utf8() {
        assert!(core::str::from_utf8(b"error: division by zero\n").is_ok());
        assert!(core::str::from_utf8(b"error: integer overflow\n").is_ok());
        assert!(core::str::from_utf8(b"error: integer cast overflow\n").is_ok());
        assert!(core::str::from_utf8(b"error: index out of bounds\n").is_ok());
        assert!(core::str::from_utf8(b"error: float-to-integer cast overflow\n").is_ok());
    }
}

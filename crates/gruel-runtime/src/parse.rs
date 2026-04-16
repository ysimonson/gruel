//! Integer parsing intrinsics
//!
//! This module implements the `@parse_i32`, `@parse_i64`, `@parse_u32`, and
//! `@parse_u64` intrinsics for parsing strings into integers.

use crate::platform;

/// Parse a string as an i32. Panics on invalid input or overflow.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_parse_i32(ptr: *const u8, len: u64) -> i32 {
    let result = __gruel_parse_i64(ptr, len);
    if result < i32::MIN as i64 || result > i32::MAX as i64 {
        parse_error_overflow();
    }
    result as i32
}

/// Parse a string as an i64. Panics on invalid input or overflow.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_parse_i64(ptr: *const u8, len: u64) -> i64 {
    if len == 0 {
        parse_error_empty();
    }

    unsafe {
        let bytes = core::slice::from_raw_parts(ptr, len as usize);
        let mut idx = 0;
        let is_negative = bytes[0] == b'-';

        if is_negative {
            idx = 1;
            if idx >= bytes.len() {
                parse_error_char();
            }
        }

        let mut result: i64 = 0;
        while idx < bytes.len() {
            let byte = bytes[idx];
            if byte < b'0' || byte > b'9' {
                parse_error_char();
            }

            let digit = (byte - b'0') as i64;

            if is_negative {
                if result < i64::MIN / 10 {
                    parse_error_overflow();
                }
                result = result * 10;
                if result < i64::MIN + digit {
                    parse_error_overflow();
                }
                result = result - digit;
            } else {
                if result > i64::MAX / 10 {
                    parse_error_overflow();
                }
                result = result * 10;
                if result > i64::MAX - digit {
                    parse_error_overflow();
                }
                result = result + digit;
            }

            idx += 1;
        }

        result
    }
}

/// Parse a string as a u32. Panics on invalid input or overflow.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_parse_u32(ptr: *const u8, len: u64) -> u32 {
    let result = __gruel_parse_u64(ptr, len);
    if result > u32::MAX as u64 {
        parse_error_overflow();
    }
    result as u32
}

/// Parse a string as a u64. Panics on invalid input or overflow.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_parse_u64(ptr: *const u8, len: u64) -> u64 {
    if len == 0 {
        parse_error_empty();
    }

    unsafe {
        let bytes = core::slice::from_raw_parts(ptr, len as usize);

        if bytes[0] == b'-' {
            parse_error_negative();
        }

        let mut result: u64 = 0;
        for &byte in bytes {
            if byte < b'0' || byte > b'9' {
                parse_error_char();
            }

            let digit = (byte - b'0') as u64;

            if result > u64::MAX / 10 {
                parse_error_overflow();
            }
            result = result * 10;
            if result > u64::MAX - digit {
                parse_error_overflow();
            }
            result = result + digit;
        }

        result
    }
}

fn parse_error_empty() -> ! {
    platform::write_stderr(b"parse error: empty string\n");
    platform::exit(101);
}

fn parse_error_char() -> ! {
    platform::write_stderr(b"parse error: invalid character\n");
    platform::exit(101);
}

fn parse_error_overflow() -> ! {
    platform::write_stderr(b"parse error: integer overflow\n");
    platform::exit(101);
}

fn parse_error_negative() -> ! {
    platform::write_stderr(b"parse error: negative value for unsigned type\n");
    platform::exit(101);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_i32_basic() {
        assert_eq!(__gruel_parse_i32(b"42".as_ptr(), 2), 42);
        assert_eq!(__gruel_parse_i32(b"0".as_ptr(), 1), 0);
        assert_eq!(__gruel_parse_i32(b"2147483647".as_ptr(), 10), 2147483647);
    }

    #[test]
    fn test_parse_i32_negative() {
        assert_eq!(__gruel_parse_i32(b"-17".as_ptr(), 3), -17);
        assert_eq!(__gruel_parse_i32(b"-2147483648".as_ptr(), 11), -2147483648);
    }

    #[test]
    fn test_parse_i64_basic() {
        assert_eq!(__gruel_parse_i64(b"42".as_ptr(), 2), 42);
        assert_eq!(
            __gruel_parse_i64(b"9223372036854775807".as_ptr(), 19),
            9223372036854775807
        );
    }

    #[test]
    fn test_parse_u32_basic() {
        assert_eq!(__gruel_parse_u32(b"42".as_ptr(), 2), 42);
        assert_eq!(__gruel_parse_u32(b"4294967295".as_ptr(), 10), 4294967295);
    }

    #[test]
    fn test_parse_u64_basic() {
        assert_eq!(__gruel_parse_u64(b"42".as_ptr(), 2), 42);
        assert_eq!(
            __gruel_parse_u64(b"18446744073709551615".as_ptr(), 20),
            18446744073709551615
        );
    }
}

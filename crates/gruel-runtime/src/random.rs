//! Random number generation intrinsics
//!
//! This module implements the `@random_u32` and `@random_u64` intrinsics for
//! generating cryptographically-secure random numbers.

use crate::platform;

/// Generate a random u32 value.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_random_u32() -> u32 {
    let mut bytes = [0u8; 4];
    platform::get_random_bytes(&mut bytes);
    u32::from_ne_bytes(bytes)
}

/// Generate a random u64 value.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_random_u64() -> u64 {
    let mut bytes = [0u8; 8];
    platform::get_random_bytes(&mut bytes);
    u64::from_ne_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_random_u32_generates_values() {
        let a = __gruel_random_u32();
        let b = __gruel_random_u32();
        let c = __gruel_random_u32();
        assert!(
            a != b || b != c,
            "random_u32 returned same value three times"
        );
    }

    #[test]
    fn test_random_u64_generates_values() {
        let a = __gruel_random_u64();
        let b = __gruel_random_u64();
        let c = __gruel_random_u64();
        assert!(
            a != b || b != c,
            "random_u64 returned same value three times"
        );
    }
}

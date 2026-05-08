//! UTF-8 validation and C-string ingestion FFI helpers.
//!
//! After ADR-0081's runtime collapse, the only String-adjacent runtime code
//! that remains is the SIMD-optional UTF-8 validator (called from the
//! prelude `String::from_utf8` body via `@utf8_validate`) and the
//! `strlen + alloc + memcpy` helper that ingests a NUL-terminated C string
//! into a fresh `Vec(u8)` (called from `String::from_c_str(_unchecked)` via
//! `@cstr_to_vec`). Everything else — equality, comparisons, mutation,
//! cloning, allocation — moved to inline Vec(T) lowerings or is composed
//! in Gruel inside `prelude/string.gruel`.

use crate::heap;

/// Minimum capacity for `__gruel_cstr_to_vec`'s allocation; matches the
/// pre-ADR-0081 `STRING_MIN_CAPACITY` so existing tests that observe the
/// post-conversion capacity continue to round up to 16.
const STRING_MIN_CAPACITY: u64 = 16;

/// `Vec(u8)` sret payload. Bit-identical to the prelude `String`'s LLVM
/// type (`{ {ptr, i64, i64} }`) — both are 24 bytes with `(ptr, len, cap)`
/// at offsets 0/8/16. The flat `repr(C)` form is what the runtime sees;
/// the wrapping into `String` is purely an LLVM type-level concern.
#[repr(C)]
pub struct VecU8Result {
    pub ptr: *mut u8,
    pub len: u64,
    pub cap: u64,
}

const _: () = {
    assert!(core::mem::size_of::<VecU8Result>() == 24);
    assert!(core::mem::align_of::<VecU8Result>() == 8);
};

/// ADR-0072: validate that `[ptr..ptr+len]` is a well-formed UTF-8 byte
/// sequence. Returns `1` if valid, `0` otherwise. Uses raw pointer reads
/// to avoid the bounds-check panics that slice indexing would emit
/// (this crate is `no_std` with no unwinder).
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_utf8_validate(ptr: *const u8, len: u64) -> u8 {
    if len == 0 {
        return 1;
    }
    let len_us = len as usize;
    let mut i = 0usize;
    while i < len_us {
        let b = unsafe { *ptr.add(i) };
        let n = if b < 0x80 {
            1usize
        } else if b & 0xE0 == 0xC0 {
            if b < 0xC2 {
                return 0; // overlong 2-byte
            }
            2usize
        } else if b & 0xF0 == 0xE0 {
            3usize
        } else if b & 0xF8 == 0xF0 {
            if b > 0xF4 {
                return 0; // codepoint > 0x10FFFF
            }
            4usize
        } else {
            return 0;
        };
        if i + n > len_us {
            return 0;
        }
        // Continuation bytes must be 10xxxxxx.
        let mut k = 1usize;
        while k < n {
            let c = unsafe { *ptr.add(i + k) };
            if c & 0xC0 != 0x80 {
                return 0;
            }
            k += 1;
        }
        if n == 3 {
            let b1 = unsafe { *ptr.add(i + 1) };
            let b2 = unsafe { *ptr.add(i + 2) };
            let cp = ((b as u32 & 0x0F) << 12) | ((b1 as u32 & 0x3F) << 6) | (b2 as u32 & 0x3F);
            if cp < 0x800 || (0xD800..=0xDFFF).contains(&cp) {
                return 0;
            }
        } else if n == 4 {
            let b1 = unsafe { *ptr.add(i + 1) };
            let b2 = unsafe { *ptr.add(i + 2) };
            let b3 = unsafe { *ptr.add(i + 3) };
            let cp = ((b as u32 & 0x07) << 18)
                | ((b1 as u32 & 0x3F) << 12)
                | ((b2 as u32 & 0x3F) << 6)
                | (b3 as u32 & 0x3F);
            if !(0x10000..=0x10FFFF).contains(&cp) {
                return 0;
            }
        }
        i += n;
    }
    1
}

/// ADR-0072: ingest a NUL-terminated C string into a fresh `Vec(u8)` with
/// strlen + alloc + memcpy. Used by `String::from_c_str(_unchecked)`.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_cstr_to_vec(out: *mut VecU8Result, p: *const u8) {
    if p.is_null() {
        unsafe {
            (*out).ptr = core::ptr::null_mut();
            (*out).len = 0;
            (*out).cap = 0;
        }
        return;
    }
    // strlen
    let mut len: u64 = 0;
    unsafe {
        while *p.add(len as usize) != 0 {
            len += 1;
        }
    }
    let cap = len.max(STRING_MIN_CAPACITY);
    let new_ptr = if cap == 0 {
        core::ptr::null_mut()
    } else {
        heap::alloc(cap, 1)
    };
    if !new_ptr.is_null() && len > 0 {
        unsafe {
            core::ptr::copy_nonoverlapping(p, new_ptr, len as usize);
        }
    }
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len;
        (*out).cap = cap;
    }
}

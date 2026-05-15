//! C-string ingestion FFI helper.
//!
//! ADR-0087's follow-up pass inlined `__gruel_utf8_validate` into the
//! prelude (`utf8_validate(s: Slice(u8)) -> bool` in
//! `prelude/runtime_wrappers.gruel`), so the only remaining
//! String-adjacent runtime symbol is `__gruel_cstr_to_vec` — the
//! `strlen + alloc + memcpy` helper that ingests a NUL-terminated C
//! string into a fresh `Vec(u8)`, called by `String::from_c_str`
//! via `@cstr_to_vec`. `@cstr_to_vec` stays as an intrinsic
//! pending whole-aggregate `@uninit` (see ADR-0087 "Rows that
//! stay").

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

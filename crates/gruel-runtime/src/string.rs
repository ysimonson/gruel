//! String runtime functions and methods.
//!
//! This module provides all runtime support for the String type:
//! - String equality comparison (`__gruel_str_eq`)
//! - Heap allocation wrappers (`__gruel_alloc`, `__gruel_free`, `__gruel_realloc`)
//! - String-specific allocation functions
//! - String constructors (`String__new`, `String__with_capacity`)
//! - String query methods (`len`, `capacity`, `is_empty`)
//! - String mutation methods (`push_str`, `push`, `clear`, `reserve`)
//! - String cloning (`String__clone`)
//! - String dropping (`__gruel_drop_String`)

use crate::heap;

/// Minimum capacity for string buffers.
pub const STRING_MIN_CAPACITY: u64 = 16;

/// The StringResult struct used for sret (struct return) convention.
#[repr(C)]
pub struct StringResult {
    pub ptr: *mut u8,
    pub len: u64,
    pub cap: u64,
}

const _: () = {
    assert!(core::mem::size_of::<StringResult>() == 24);
    assert!(core::mem::align_of::<StringResult>() == 8);
};

// =============================================================================
// String Equality
// =============================================================================

/// String equality comparison.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_str_eq(ptr1: *const u8, len1: u64, ptr2: *const u8, len2: u64) -> u8 {
    if len1 != len2 {
        return 0;
    }
    if ptr1 == ptr2 {
        return 1;
    }
    for i in 0..len1 as usize {
        let b1 = unsafe { *ptr1.add(i) };
        let b2 = unsafe { *ptr2.add(i) };
        if b1 != b2 {
            return 0;
        }
    }
    1
}

// =============================================================================
// String Ordering Comparison
// =============================================================================

/// Lexicographic byte comparison of two strings.
/// Returns -1 if s1 < s2, 0 if s1 == s2, 1 if s1 > s2.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_str_cmp(ptr1: *const u8, len1: u64, ptr2: *const u8, len2: u64) -> i8 {
    let min_len = if len1 < len2 { len1 } else { len2 };
    for i in 0..min_len as usize {
        let b1 = unsafe { *ptr1.add(i) };
        let b2 = unsafe { *ptr2.add(i) };
        if b1 < b2 {
            return -1;
        }
        if b1 > b2 {
            return 1;
        }
    }
    if len1 < len2 {
        -1
    } else if len1 > len2 {
        1
    } else {
        0
    }
}

// =============================================================================
// Heap Allocation Wrappers
// =============================================================================

/// Allocate memory from the heap.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_alloc(size: u64, align: u64) -> *mut u8 {
    heap::alloc(size, align)
}

/// Free memory previously allocated by `__gruel_alloc`.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_free(ptr: *mut u8, size: u64, align: u64) {
    heap::free(ptr, size, align)
}

/// Reallocate memory to a new size.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_realloc(
    ptr: *mut u8,
    old_size: u64,
    new_size: u64,
    align: u64,
) -> *mut u8 {
    heap::realloc(ptr, old_size, new_size, align)
}

// =============================================================================
// String-Specific Allocation Functions
// =============================================================================

/// Allocate a new string buffer with the given capacity.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_string_alloc(cap: u64) -> *mut u8 {
    let actual_cap = if cap < STRING_MIN_CAPACITY {
        STRING_MIN_CAPACITY
    } else {
        cap
    };
    heap::alloc(actual_cap, 1)
}

/// Reallocate a string buffer to a new capacity (2x growth strategy).
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_string_realloc(ptr: *mut u8, old_cap: u64, new_cap: u64) -> *mut u8 {
    let grown_cap = old_cap.saturating_mul(2);
    let actual_cap = new_cap.max(grown_cap).max(STRING_MIN_CAPACITY);
    heap::realloc(ptr, old_cap, actual_cap, 1)
}

/// Clone a string by allocating a new buffer and copying the content.
#[unsafe(no_mangle)]
pub extern "C" fn __gruel_string_clone(ptr: *const u8, len: u64) -> *mut u8 {
    let cap = len.max(STRING_MIN_CAPACITY);
    let new_ptr = heap::alloc(cap, 1);
    if new_ptr.is_null() {
        return new_ptr;
    }
    if len > 0 && !ptr.is_null() {
        unsafe {
            core::ptr::copy_nonoverlapping(ptr, new_ptr, len as usize);
        }
    }
    new_ptr
}

/// Drop a String, freeing its heap buffer if heap-allocated.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn __gruel_drop_String(ptr: *mut u8, _len: u64, cap: u64) {
    if cap > 0 {
        heap::free(ptr, cap, 1);
    }
}

// =============================================================================
// String Construction Functions
// =============================================================================

/// Create an empty String with no allocation.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__new(out: *mut StringResult) {
    unsafe {
        (*out).ptr = core::ptr::null_mut();
        (*out).len = 0;
        (*out).cap = 0;
    }
}

/// Create an empty String with pre-allocated capacity.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__with_capacity(out: *mut StringResult, requested_cap: u64) {
    let actual_cap = if requested_cap < STRING_MIN_CAPACITY {
        STRING_MIN_CAPACITY
    } else {
        requested_cap
    };
    let ptr = heap::alloc(actual_cap, 1);
    unsafe {
        (*out).ptr = ptr;
        (*out).len = 0;
        (*out).cap = actual_cap;
    }
}

/// ADR-0071: encode `codepoint` as UTF-8 (1–4 bytes) into `out_bytes` and
/// return the byte count.  `codepoint` must be a valid Unicode scalar value
/// (caller's responsibility — char values from sema satisfy this).
///
/// Uses raw pointer writes to avoid the bounds-check panics that the array
/// `[u8; 4]` indexing would emit (this crate is `no_std` with no unwinder).
fn encode_utf8(codepoint: u32, out_bytes: *mut u8) -> usize {
    unsafe {
        if codepoint < 0x80 {
            *out_bytes.add(0) = codepoint as u8;
            1
        } else if codepoint < 0x800 {
            *out_bytes.add(0) = 0xC0 | (codepoint >> 6) as u8;
            *out_bytes.add(1) = 0x80 | (codepoint & 0x3F) as u8;
            2
        } else if codepoint < 0x10000 {
            *out_bytes.add(0) = 0xE0 | (codepoint >> 12) as u8;
            *out_bytes.add(1) = 0x80 | ((codepoint >> 6) & 0x3F) as u8;
            *out_bytes.add(2) = 0x80 | (codepoint & 0x3F) as u8;
            3
        } else {
            *out_bytes.add(0) = 0xF0 | (codepoint >> 18) as u8;
            *out_bytes.add(1) = 0x80 | ((codepoint >> 12) & 0x3F) as u8;
            *out_bytes.add(2) = 0x80 | ((codepoint >> 6) & 0x3F) as u8;
            *out_bytes.add(3) = 0x80 | (codepoint & 0x3F) as u8;
            4
        }
    }
}

/// ADR-0071: build a String containing the UTF-8 encoding of `codepoint`.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__from_char(out: *mut StringResult, codepoint: u32) {
    let cap = STRING_MIN_CAPACITY;
    let ptr = heap::alloc(cap, 1);
    let n = encode_utf8(codepoint, ptr);
    unsafe {
        (*out).ptr = ptr;
        (*out).len = n as u64;
        (*out).cap = cap;
    }
}

/// ADR-0071: append the UTF-8 encoding of `codepoint` (1–4 bytes) to the string.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__push_char(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    codepoint: u32,
) {
    // 4-byte upper bound for any UTF-8 encoding.
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, 4);
    let n = unsafe { encode_utf8(codepoint, new_ptr.add(len as usize)) };
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len + n as u64;
        (*out).cap = new_cap;
    }
}

// =============================================================================
// String Query Methods
// =============================================================================

/// Get the length of a String in bytes.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__len(_ptr: *const u8, len: u64, _cap: u64) -> u64 {
    len
}

/// Get the capacity of a String in bytes.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__capacity(_ptr: *const u8, _len: u64, cap: u64) -> u64 {
    cap
}

/// Check if a String is empty.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__is_empty(_ptr: *const u8, len: u64, _cap: u64) -> u8 {
    if len == 0 { 1 } else { 0 }
}

/// Check if a String contains a substring.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__contains(
    ptr: *const u8,
    len: u64,
    _cap: u64,
    needle_ptr: *const u8,
    needle_len: u64,
    _needle_cap: u64,
) -> u8 {
    if needle_len == 0 {
        return 1;
    }
    if needle_len > len {
        return 0;
    }
    let haystack = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
    let needle = unsafe { core::slice::from_raw_parts(needle_ptr, needle_len as usize) };
    for i in 0..=(len - needle_len) as usize {
        if &haystack[i..i + needle_len as usize] == needle {
            return 1;
        }
    }
    0
}

/// Check if a String starts with a prefix.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__starts_with(
    ptr: *const u8,
    len: u64,
    _cap: u64,
    prefix_ptr: *const u8,
    prefix_len: u64,
    _prefix_cap: u64,
) -> u8 {
    if prefix_len == 0 {
        return 1;
    }
    if prefix_len > len {
        return 0;
    }
    let haystack = unsafe { core::slice::from_raw_parts(ptr, prefix_len as usize) };
    let prefix = unsafe { core::slice::from_raw_parts(prefix_ptr, prefix_len as usize) };
    if haystack == prefix { 1 } else { 0 }
}

/// Check if a String ends with a suffix.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__ends_with(
    ptr: *const u8,
    len: u64,
    _cap: u64,
    suffix_ptr: *const u8,
    suffix_len: u64,
    _suffix_cap: u64,
) -> u8 {
    if suffix_len == 0 {
        return 1;
    }
    if suffix_len > len {
        return 0;
    }
    let offset = (len - suffix_len) as usize;
    let tail = unsafe { core::slice::from_raw_parts(ptr.add(offset), suffix_len as usize) };
    let suffix = unsafe { core::slice::from_raw_parts(suffix_ptr, suffix_len as usize) };
    if tail == suffix { 1 } else { 0 }
}

/// Concatenate two strings, returning a new string.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__concat(
    out: *mut StringResult,
    ptr: *const u8,
    len: u64,
    _cap: u64,
    other_ptr: *const u8,
    other_len: u64,
    _other_cap: u64,
) {
    let total_len = len + other_len;
    if total_len == 0 {
        unsafe {
            (*out).ptr = core::ptr::null_mut();
            (*out).len = 0;
            (*out).cap = 0;
        }
        return;
    }
    let new_cap = total_len.max(STRING_MIN_CAPACITY);
    let new_ptr = heap::alloc(new_cap, 1);
    if new_ptr.is_null() {
        unsafe {
            (*out).ptr = core::ptr::null_mut();
            (*out).len = 0;
            (*out).cap = 0;
        }
        return;
    }
    if len > 0 && !ptr.is_null() {
        unsafe {
            core::ptr::copy_nonoverlapping(ptr, new_ptr, len as usize);
        }
    }
    if other_len > 0 && !other_ptr.is_null() {
        unsafe {
            core::ptr::copy_nonoverlapping(
                other_ptr,
                new_ptr.add(len as usize),
                other_len as usize,
            );
        }
    }
    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = total_len;
        (*out).cap = new_cap;
    }
}

// =============================================================================
// String Clone Method
// =============================================================================

/// Clone a String, creating a deep copy.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__clone(out: *mut StringResult, ptr: *const u8, len: u64, _cap: u64) {
    let new_cap = len.max(STRING_MIN_CAPACITY);
    let new_ptr = heap::alloc(new_cap, 1);

    if new_ptr.is_null() {
        unsafe {
            (*out).ptr = core::ptr::null_mut();
            (*out).len = 0;
            (*out).cap = 0;
        }
        return;
    }

    if len > 0 && !ptr.is_null() {
        unsafe {
            core::ptr::copy_nonoverlapping(ptr, new_ptr, len as usize);
        }
    }

    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len;
        (*out).cap = new_cap;
    }
}

// =============================================================================
// String Mutation Methods
// =============================================================================

/// Append another string's content to this string.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__push_str(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    other_ptr: *const u8,
    other_len: u64,
    _other_cap: u64,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, other_len);

    if other_len > 0 && !other_ptr.is_null() {
        unsafe {
            core::ptr::copy_nonoverlapping(
                other_ptr,
                new_ptr.add(len as usize),
                other_len as usize,
            );
        }
    }

    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len + other_len;
        (*out).cap = new_cap;
    }
}

/// Append a single byte to this string.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__push(out: *mut StringResult, ptr: *mut u8, len: u64, cap: u64, byte: u8) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, 1);

    unsafe {
        *new_ptr.add(len as usize) = byte;
    }

    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len + 1;
        (*out).cap = new_cap;
    }
}

/// Clear the string content, keeping capacity.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__clear(out: *mut StringResult, ptr: *mut u8, _len: u64, cap: u64) {
    unsafe {
        (*out).ptr = ptr;
        (*out).len = 0;
        (*out).cap = cap;
    }
}

/// Reserve additional capacity in the string.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn String__reserve(
    out: *mut StringResult,
    ptr: *mut u8,
    len: u64,
    cap: u64,
    additional: u64,
) {
    let (new_ptr, new_cap) = string_ensure_capacity(ptr, len, cap, additional);

    unsafe {
        (*out).ptr = new_ptr;
        (*out).len = len;
        (*out).cap = new_cap;
    }
}

// =============================================================================
// Internal Helper
// =============================================================================

/// Ensure a string has enough capacity for additional bytes.
///
/// Handles heap promotion (cap == 0) and growth.
/// Returns (new_ptr, new_cap) with capacity >= len + additional.
#[inline]
fn string_ensure_capacity(ptr: *mut u8, len: u64, cap: u64, additional: u64) -> (*mut u8, u64) {
    let required = len.saturating_add(additional);

    if cap == 0 {
        // Heap promotion: allocate new buffer and copy existing content
        let new_cap = required.max(STRING_MIN_CAPACITY);
        let new_ptr = heap::alloc(new_cap, 1);
        if new_ptr.is_null() {
            return (core::ptr::null_mut(), 0);
        }
        if len > 0 && !ptr.is_null() {
            unsafe {
                core::ptr::copy_nonoverlapping(ptr as *const u8, new_ptr, len as usize);
            }
        }
        (new_ptr, new_cap)
    } else if required > cap {
        // Need to grow
        let new_ptr = heap::realloc(ptr, cap, required, 1);
        if new_ptr.is_null() {
            return (core::ptr::null_mut(), 0);
        }
        let grown_cap = cap.saturating_mul(2);
        let new_cap = required.max(grown_cap).max(STRING_MIN_CAPACITY);
        (new_ptr, new_cap)
    } else {
        (ptr, cap)
    }
}

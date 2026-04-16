//! Heap allocation for Gruel programs.
//!
//! Thin wrappers around libc's malloc/free/realloc. The actual allocator
//! implementation comes from whatever libc is linked (musl, glibc, etc.).

use crate::platform;

/// Allocate memory from the heap.
///
/// Returns a pointer to at least `size` bytes of memory, or null on failure.
/// The `align` parameter is accepted for API compatibility but not enforced
/// beyond libc's default alignment (typically 16 bytes).
pub fn alloc(size: u64, _align: u64) -> *mut u8 {
    if size == 0 {
        return core::ptr::null_mut();
    }
    unsafe { platform::malloc(size as usize) }
}

/// Free memory previously allocated by `alloc`.
pub fn free(ptr: *mut u8, _size: u64, _align: u64) {
    if !ptr.is_null() {
        unsafe { platform::free(ptr) }
    }
}

/// Reallocate memory to a new size.
///
/// - If `ptr` is null: behaves like `alloc(new_size, align)`
/// - If `new_size` is 0: frees the memory and returns null
/// - Otherwise: grows or shrinks the allocation, copying data as needed
pub fn realloc(ptr: *mut u8, _old_size: u64, new_size: u64, _align: u64) -> *mut u8 {
    if ptr.is_null() {
        return alloc(new_size, 1);
    }
    if new_size == 0 {
        unsafe { platform::free(ptr) }
        return core::ptr::null_mut();
    }
    unsafe { platform::realloc(ptr, new_size as usize) }
}

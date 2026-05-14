//! Heap allocation helpers for the Gruel runtime itself.
//!
//! ADR-0087 Phase 4 retired the `__gruel_alloc` / `__gruel_free` /
//! `__gruel_realloc` FFI entry points — the user-facing surface is
//! now the `mem_alloc` / `mem_free` / `mem_realloc` Gruel prelude
//! fns (see `prelude/runtime_wrappers.gruel`), which call libc
//! `malloc` / `free` / `realloc` directly via Phase 1's
//! `link_extern("c")` block. The in-crate Rust helpers below stay
//! because the runtime still needs to allocate from its own code
//! (the `@spawn` thunk for the arg/return boxes, primarily); those
//! callers go through `platform::malloc` etc., not the deleted
//! `__gruel_*` shims.

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

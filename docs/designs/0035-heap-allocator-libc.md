---
id: 0035
title: Heap Allocator - Use libc malloc
status: implemented
tags: [runtime, memory, allocator]
feature-flag: runtime-heap
created: 2026-01-04
accepted: 2026-01-04
implemented: 2026-01-04
spec-sections: []
superseded-by:
---

# ADR-0035: Heap Allocator - Use libc malloc

## Status

Implemented

## Summary

Supersedes ADR-0011. Instead of the mmap-backed bump allocator proposed in ADR-0011, the Gruel runtime delegates heap allocation to libc's `malloc`/`free`/`realloc`. The public API (`__gruel_alloc`, `__gruel_realloc`, `__gruel_free`) is unchanged.

## Context

ADR-0011 proposed a no-libc bump allocator backed by `mmap`/`munmap` syscalls. By the time heap allocation was needed (when `String` landed), gruel-runtime already linked libc for I/O:

- `getline` (ADR-0021, `@read_line`)
- `write` (stdout/stderr output)
- `exit` (process termination)

The no-libc constraint from ADR-0011 was therefore already relaxed. A custom bump allocator adds maintenance burden without benefit when libc is present.

### Why Not the Bump Allocator

The bump allocator's "no individual free" trade-off is a correctness hazard now that Gruel has destructors (ADR-0010): `__gruel_free` must actually release memory, not be a no-op, or programs leak on every drop. A bump allocator would require a complete rewrite to support real frees anyway.

## Decision

Thin wrappers around libc `malloc`/`free`/`realloc`:

```rust
pub fn alloc(size: u64, _align: u64) -> *mut u8 {
    if size == 0 { return core::ptr::null_mut(); }
    unsafe { platform::malloc(size as usize) }
}

pub fn free(ptr: *mut u8, _size: u64, _align: u64) {
    if !ptr.is_null() { unsafe { platform::free(ptr) } }
}

pub fn realloc(ptr: *mut u8, _old_size: u64, new_size: u64, _align: u64) -> *mut u8 {
    if new_size == 0 { unsafe { platform::free(ptr) }; return core::ptr::null_mut(); }
    unsafe { platform::realloc(ptr, new_size as usize) }
}
```

The `size`, `old_size`, and `align` parameters are accepted for API compatibility with the interface defined in ADR-0011 but are not forwarded to libc (libc tracks size internally; libc's default alignment of 16 bytes satisfies all current use cases).

### Public API (unchanged from ADR-0011)

```rust
#[no_mangle]
pub extern "C" fn __gruel_alloc(size: u64, align: u64) -> *mut u8

#[no_mangle]
pub extern "C" fn __gruel_realloc(ptr: *mut u8, old_size: u64, new_size: u64, align: u64) -> *mut u8

#[no_mangle]
pub extern "C" fn __gruel_free(ptr: *mut u8, size: u64, align: u64)
```

## Implementation Phases

- [x] **Phase 1: libc platform wrappers** - Add `malloc`/`free`/`realloc` to each platform module (`x86_64_linux.rs`, `aarch64_linux.rs`, `aarch64_macos.rs`)
- [x] **Phase 2: Runtime wrappers** - Implement `__gruel_alloc`, `__gruel_realloc`, `__gruel_free` delegating to libc
- [x] **Phase 3: Integration** - Wire up for `String` and other heap-allocated types

## Consequences

### Positive

- **Zero maintenance**: libc handles alignment, thread safety, sizing, and platform differences
- **Correct frees**: `__gruel_free` actually releases memory, required for destructors
- **Battle-tested**: libc malloc is reliable across all supported platforms

### Negative

- **libc dependency**: Abandons the no-libc goal from ADR-0011 (already abandoned by I/O)
- **No alignment guarantee beyond 16 bytes**: Acceptable for all current types

### Neutral

- **API unchanged**: Callers use the same three functions as proposed in ADR-0011

## References

- [ADR-0011: Runtime Heap](0011-runtime-heap.md) — Original bump allocator proposal (superseded by this ADR)
- [ADR-0010: Destructors](0010-destructors.md) — Requires real `free`, not a no-op
- [ADR-0021: Stdin Input](0021-stdin-input.md) — Established libc dependency in gruel-runtime

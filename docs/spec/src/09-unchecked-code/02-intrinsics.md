+++
title = "Unchecked Intrinsics"
weight = 2
template = "spec/page.html"
+++

# Unchecked Intrinsics

This section describes intrinsics that require a checked block.

## Syscall Intrinsic

{{ rule(id="9.2:1", cat="normative") }}

The `@syscall` intrinsic performs a direct system call to the operating system.

{{ rule(id="9.2:2", cat="syntax") }}

```ebnf
syscall_intrinsic = "@syscall" "(" syscall_number { "," argument } ")" ;
syscall_number = expression ;
argument = expression ;
```

{{ rule(id="9.2:3", cat="legality-rule") }}

The `@syscall` intrinsic takes at least one argument (the syscall number) and at most seven arguments (syscall number plus six syscall arguments). All arguments must be of type `u64`.

{{ rule(id="9.2:4", cat="dynamic-semantics") }}

The `@syscall` intrinsic returns an `i64` value representing the result of the syscall. On Linux x86-64, negative values typically indicate errors. The exact behavior depends on the syscall being invoked and the platform.

{{ rule(id="9.2:5", cat="informative") }}

Syscall numbers and conventions differ between operating systems. Linux x86-64 syscall numbers are different from macOS aarch64 syscall numbers. Users should consult platform-specific documentation.

{{ rule(id="9.2:6", cat="example") }}

```gruel
fn main() -> i32 {
    checked {
        // Linux x86-64: write(fd=1, buf, len)
        let result = @syscall(1_u64, 1_u64, msg_ptr, msg_len);

        // Linux x86-64: exit_group(code)
        @syscall(231_u64, 0_u64);
    };
    0
}
```

## Null Pointer Intrinsic

{{ rule(id="9.2:7", cat="normative") }}

The `@null_ptr()` intrinsic creates a null pointer. It takes no arguments and returns a pointer whose address is zero. The result type is inferred from context and must be a pointer type (`Ptr(T)` or `MutPtr(T)`).

{{ rule(id="9.2:8", cat="example") }}

```gruel
fn main() -> i32 {
    let p: Ptr(i32) = checked { @null_ptr() };
    0
}
```

## Null Check Intrinsic

{{ rule(id="9.2:9", cat="normative") }}

The `@is_null(p)` intrinsic checks whether a pointer is null. It takes one argument of any pointer type (`Ptr(T)` or `MutPtr(T)`) and returns `bool`. The result is `true` if the pointer address is zero, `false` otherwise.

{{ rule(id="9.2:10", cat="example") }}

```gruel
fn main() -> i32 {
    let p: Ptr(i32) = checked { @null_ptr() };
    if checked { @is_null(p) } { 1 } else { 0 }
}
```

## Pointer Copy Intrinsic

{{ rule(id="9.2:11", cat="normative") }}

The `@ptr_copy(dst, src, count)` intrinsic copies `count` elements from the memory at `src` to the memory at `dst`. The first argument must be `MutPtr(T)`, the second must be `Ptr(T)` or `MutPtr(T)` with the same pointee type, and the third must be `u64`. The intrinsic returns `()`.

{{ rule(id="9.2:12", cat="undefined-behavior") }}

It is undefined behavior if the source and destination memory regions overlap, if either pointer is null, or if either pointer does not point to a valid allocation of at least `count` elements.

{{ rule(id="9.2:13", cat="example") }}

```gruel
fn main() -> i32 {
    let src: [i32; 3] = [10, 20, 30];
    let mut dst: [i32; 3] = [0, 0, 0];
    let count: u64 = 3;
    let s: Ptr(i32) = checked { @raw(src[0]) };
    let d: MutPtr(i32) = checked { @raw_mut(dst[0]) };
    checked { @ptr_copy(d, s, count) };
    dst[1]
}
```

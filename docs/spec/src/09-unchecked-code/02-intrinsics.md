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

## Pointer Methods (ADR-0063)

> **Migration note (informative):** The legacy `@…` pointer intrinsics
> (`@ptr_read`, `@ptr_write`, `@ptr_offset`, `@ptr_to_int`,
> `@int_to_ptr`, `@null_ptr`, `@is_null`, `@ptr_copy`, `@raw`,
> `@raw_mut`) are accepted in parallel during the migration to ADR-0063.
> Phase 6 of ADR-0063 removes them; the new surface form below is the
> only spelling after that.

{{ rule(id="9.2:14", cat="normative") }}

The operations on `Ptr(T)` / `MutPtr(T)` defined by ADR-0028 are also exposed as method calls on a pointer value and as associated-function calls on a fully-applied pointer type. The method / associated-function form is **subject to the same `checked`-block requirement** the corresponding intrinsic has. The two forms are semantically identical; only the spelling differs.

| Form | Defined on | Signature |
|------|------------|-----------|
| `p.read()` | `Ptr(T)`, `MutPtr(T)` | `(self) -> T` |
| `p.write(v)` | `MutPtr(T)` only | `(self, v: T) -> ()` |
| `p.offset(n)` | `Ptr(T)`, `MutPtr(T)` | `(self, n: i64) -> Self` |
| `p.is_null()` | `Ptr(T)`, `MutPtr(T)` | `(self) -> bool` |
| `p.to_int()` | `Ptr(T)`, `MutPtr(T)` | `(self) -> u64` |
| `p.copy_from(src, n)` | `MutPtr(T)` | `(self, src: Ptr(T) \| MutPtr(T), n: u64) -> ()` |
| `Ptr(T)::from(r)` | `Ptr(T)` | `(r: Ref(T)) -> Ptr(T)` |
| `MutPtr(T)::from(r)` | `MutPtr(T)` | `(r: MutRef(T)) -> MutPtr(T)` |
| `Ptr(T)::null()` | `Ptr(T)` | `() -> Ptr(T)` |
| `MutPtr(T)::null()` | `MutPtr(T)` | `() -> MutPtr(T)` |
| `Ptr(T)::from_int(addr)` | `Ptr(T)` | `(addr: u64) -> Ptr(T)` |
| `MutPtr(T)::from_int(addr)` | `MutPtr(T)` | `(addr: u64) -> MutPtr(T)` |

{{ rule(id="9.2:15", cat="syntax") }}

```ebnf
ptr_method_call = expr "." IDENT "(" [ args ] ")" ;
ptr_assoc_fn_call = ptr_type "::" IDENT "(" [ args ] ")" ;
```

{{ rule(id="9.2:16", cat="example") }}

```gruel
fn main() -> i32 {
    let mut x: i32 = 41;
    checked {
        let p = MutPtr(i32)::from(&mut x);
        p.write(p.read() + 1);
    };
    x  // 42
}
```

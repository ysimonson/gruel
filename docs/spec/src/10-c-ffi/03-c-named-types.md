+++
title = "C named primitive types"
weight = 3
template = "spec/page.html"
+++

# C named primitive types (ADR-0086)

{{ rule(id="10.3:1", cat="normative") }}
The C named primitive types are thirteen built-in type names introduced by ADR-0086: `c_schar`, `c_short`, `c_int`, `c_long`, `c_longlong`, `c_uchar`, `c_ushort`, `c_uint`, `c_ulong`, `c_ulonglong`, `c_float`, `c_double`, and `c_void`. They are gated behind the `c_ffi_extras` preview feature.

{{ rule(id="10.3:2", cat="normative") }}
The twelve arithmetic C named types are target-resolved. On every currently-blessed target (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin` — all LP64), the resolutions are:

| Gruel type | C type | Width (bits) | Signed |
|---|---|---|---|
| `c_schar` | `signed char` | 8 | yes |
| `c_uchar` | `unsigned char` | 8 | no |
| `c_short` | `short` | 16 | yes |
| `c_ushort` | `unsigned short` | 16 | no |
| `c_int` | `int` | 32 | yes |
| `c_uint` | `unsigned int` | 32 | no |
| `c_long` | `long` | 64 | yes |
| `c_ulong` | `unsigned long` | 64 | no |
| `c_longlong` | `long long` | 64 | yes |
| `c_ulonglong` | `unsigned long long` | 64 | no |
| `c_float` | `float` | 32 | — |
| `c_double` | `double` | 64 | — |

{{ rule(id="10.3:3", cat="normative") }}
Each C named arithmetic type is distinct from every native Gruel arithmetic type and from every other C named type. A value of one cannot appear in a context that expects another without an explicit `as` conversion.

{{ rule(id="10.3:4", cat="normative") }}
Integer literals coerce to a C named integer type when the context fixes one (for example a `let` annotation or a function return type). Float literals coerce to `c_float` or `c_double` analogously. The literal must fit the target-resolved width of the type.

{{ rule(id="10.3:5", cat="normative") }}
All twelve C named arithmetic types are permitted across the FFI boundary (parameters of, and returns from, `@mark(c) fn` declarations and items inside `link_extern` blocks).

{{ rule(id="10.3:6", cat="normative") }}
`c_void` is an *incomplete* type. It has no values, no size, and no alignment. A `c_void` name is well-formed only when it appears as the pointee of `Ptr(c_void)` or `MutPtr(c_void)`. Bare `c_void` as the type of a `let` binding, function parameter, function return, struct field, or enum variant field is a compile-time error.

{{ rule(id="10.3:7", cat="normative") }}
`Ptr(c_void)` and `MutPtr(c_void)` are permitted across the FFI boundary and elsewhere a raw pointer type is allowed. They correspond to C `const void *` and `void *`, respectively.

{{ rule(id="10.3:8", cat="example") }}
```gruel
link_extern("c") {
    fn abs(x: c_int) -> c_int;
    fn malloc(size: usize) -> MutPtr(c_void);
    fn free(p: MutPtr(c_void)) -> ();
}

fn main() -> i32 {
    let x: c_int = 42;
    let p: MutPtr(c_void) = malloc(64);
    free(p);
    let y: c_int = abs(x);
    0
}
```

+++
title = "@mark(unchecked) directive"
weight = 3
template = "spec/page.html"
+++

# `@mark(unchecked)` directive

ADR-0088 introduces `@mark(unchecked)` as the uniform spelling for
declaring a fn unchecked. It replaces the legacy `unchecked` keyword
and extends the surface to struct/enum methods, interface method
signatures, and FFI imports. During the migration window (ADR-0088
Phases 1–5), both spellings are accepted on top-level fns; methods
and FFI imports accept only the directive form. Stabilisation removes
the legacy keyword (Phase 6).

This section is gated by the `unchecked_fn_extensions` preview
feature until ADR-0088 stabilises.

## Directive syntax

{{ rule(id="9.2:1", cat="normative") }}

A top-level function declaration **MAY** carry `@mark(unchecked)` in
its directive list. The directive is equivalent to the legacy
`unchecked` keyword: every caller of the function must wrap the call
in a `checked { }` block (see 9.1:3).

{{ rule(id="9.2:2", cat="normative") }}

A method declaration (in a regular struct/enum `impl`-style body, in
an anonymous-struct literal, or attached to an interface as a
required method) **MAY** carry `@mark(unchecked)` in its directive
list. The same `checked { }` requirement applies to every call site
of an `@mark(unchecked)` method (9.1:3 generalised to methods).

{{ rule(id="9.2:3", cat="legality-rule") }}

`@mark(unchecked)` is a compile-time error when applied to a
destructor method (`fn __drop`). Drop glue runs implicitly at scope
exit; no caller-side `checked { }` is available to gate it.

{{ rule(id="9.2:4", cat="example") }}

```gruel
@mark(unchecked)
fn dangerous_op() -> i32 { 42 }

struct Foo {
    val: i32,

    @mark(unchecked)
    pub fn raw_get(self) -> i32 { self.val }
}

fn main() -> i32 {
    let f = Foo { val: 42 };
    checked { dangerous_op() + f.raw_get() }
}
```

## Built-in pointer methods classified by the unchecked rule

{{ rule(id="9.2:5", cat="normative") }}

The methods and associated functions on `Ptr(T)` and `MutPtr(T)`
(ADR-0063, ADR-0088) are classified by the unchecked rule:

| Surface | `is_unchecked` |
|---|---|
| `p.read()`, `p.read_volatile()` | true |
| `p.write(v)`, `p.write_volatile(v)` (`MutPtr` only) | true |
| `p.offset(n)` | true |
| `p.copy_from(src, n)` (`MutPtr` only) | true |
| `p.is_null()` | false |
| `p.to_int()` | false |
| `Ptr(T)::from(&r)`, `MutPtr(T)::from(&mut r)` | false |
| `Ptr(T)::null()`, `MutPtr(T)::null()` | false |
| `Ptr(T)::from_int(addr)`, `MutPtr(T)::from_int(addr)` | false |

{{ rule(id="9.2:6", cat="legality-rule") }}

An opaque-token operation (`is_null`, `to_int`) does not require a
`checked { }` block: the body reads the address as a number or
compares it against null, with no dependency on the pointer pointing
to anything valid.

{{ rule(id="9.2:7", cat="legality-rule") }}

A constructor that does not itself dereference (`from`, `null`,
`from_int`) does not require a `checked { }` block. Caller-side
hazards (use-after-free, OOB) are gated at the eventual `read` /
`write` / `offset` / `copy_from` call site, all of which are
`@mark(unchecked)`.

{{ rule(id="9.2:8", cat="legality-rule") }}

An unchecked pointer method (`read`, `read_volatile`, `write`,
`write_volatile`, `offset`, `copy_from`) outside a `checked { }`
block is a compile-time error. The diagnostic shape is the same as
for any other `@mark(unchecked)` method call.

## `char::from_u32_unchecked`

{{ rule(id="9.2:9", cat="normative") }}

The compiler-recognised `char::from_u32_unchecked(n: u32) -> char`
is `@mark(unchecked)`. The caller asserts that `n` is a valid
Unicode scalar value (in 0..=0x10FFFF, excluding the surrogate
range 0xD800..=0xDFFF). The validating variant
`char::from_u32(n: u32) -> Result(char, u32)` is the checked
default for u32-to-char conversion.

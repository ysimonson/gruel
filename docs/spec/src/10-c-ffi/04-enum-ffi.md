+++
title = "Enum FFI"
weight = 4
template = "spec/page.html"
+++

# `@mark(c) enum` (ADR-0086)

{{ rule(id="10.4:1", cat="normative") }}
The `@mark(c)` marker, originally restricted to functions and structs by ADR-0085, additionally applies to enum declarations. `@mark(c) enum` types are gated behind the `c_ffi_extras` preview feature.

{{ rule(id="10.4:2", cat="normative") }}
A `@mark(c) enum` uses [`c_int`](@/10-c-ffi/03-c-named-types.md) as its discriminant type, regardless of variant count. This matches C's default `enum` discriminant convention.

{{ rule(id="10.4:3", cat="normative") }}
A field-less `@mark(c) enum` (every variant is a unit variant) lowers to a bare `c_int` value: size = `sizeof(c_int)`, alignment = `alignof(c_int)`. Niche optimisation is disabled.

{{ rule(id="10.4:4", cat="normative") }}
A field-less `@mark(c) enum` is permitted at the FFI boundary as a parameter or return type of any `@mark(c)` fn or `link_extern` item declaration.

{{ rule(id="10.4:5", cat="normative") }}
Data-carrying `@mark(c) enum` types (at least one variant carries fields) are not permitted in this revision. They are reserved for the next ADR-0086 phase, which will introduce a C tagged-union layout. Until then, such enums are rejected at the FFI boundary with a compile-time diagnostic.

{{ rule(id="10.4:6", cat="normative") }}
A non-`@mark(c)` enum cannot cross the FFI boundary by value. The diagnostic surfaces the type name and the FFI position.

{{ rule(id="10.4:7", cat="example") }}
```gruel
@mark(c) enum Color {
    Red,
    Green,
    Blue,
}

link_extern("foo") {
    fn translate(c: Color) -> Color;
}

fn main() -> i32 {
    let red: Color = Color::Red;
    0
}
```

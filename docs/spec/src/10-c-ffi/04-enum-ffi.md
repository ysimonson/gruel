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
A data-carrying `@mark(c) enum` (at least one variant carries fields) uses the C tagged-union layout: the discriminant of type `c_int` sits at offset 0, followed by a payload region. The payload region starts at offset `max(alignof(c_int), max(alignof(variant_i)))` and is sized to `max(size(variant_i))`. Total enum size is `payload_offset + payload_size`, rounded up to enum alignment = `max(alignof(c_int), max(alignof(variant_i)))`. Niche optimisation is disabled. Each variant payload field must itself satisfy the C-FFI-type rules from §10.1 — non-FFI fields are rejected with a diagnostic that names the offending variant and field.

{{ rule(id="10.4:6", cat="normative") }}
A non-`@mark(c)` enum cannot cross the FFI boundary by value. The diagnostic surfaces the type name and the FFI position.

{{ rule(id="10.4:7", cat="example") }}
```gruel
@mark(c) enum Event {
    Quit,
    KeyPress(c_int),
    MouseMove { x: c_int, y: c_int },
}

link_extern("foo") {
    fn poll_event() -> Event;
}

fn main() -> i32 {
    let red: Event = Event::Quit;
    0
}
```

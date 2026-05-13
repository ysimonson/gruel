+++
title = "link_extern blocks"
weight = 2
template = "spec/page.html"
+++

# `link_extern("libname") { … }` blocks (ADR-0085)

{{ rule(id="10.2:1", cat="syntax") }}
A `link_extern` item form has the shape `link_extern "(" string-literal ")" "{" item* "}"`. The string literal names a library; the body is a sequence of body-less fn declarations.

{{ rule(id="10.2:2", cat="normative") }}
Each fn declared inside a `link_extern` block is implicitly an extern declaration: the symbol resolves at link time, no body is permitted, and the call uses the C calling convention (implicit `@mark(c)`).

{{ rule(id="10.2:3", cat="normative") }}
Body-less fn declarations (`fn name(...) [-> type] ;`) are only permitted inside a `link_extern` block. A body-less fn at top level is a compile-time error.

{{ rule(id="10.2:4", cat="normative") }}
A fn declared inside a `link_extern` block must not carry a body. A fn with a body inside a `link_extern` block is a compile-time error.

{{ rule(id="10.2:5", cat="normative") }}
The library name passed to `link_extern("…")` must be a non-empty string.

{{ rule(id="10.2:6", cat="normative") }}
`link_extern` blocks do not nest.

{{ rule(id="10.2:7", cat="example") }}
```gruel
link_extern("m") {
    fn sin(x: f64) -> f64;
    fn cos(x: f64) -> f64;
}

link_extern("c") {
    fn abs(x: i32) -> i32;
}
```

{{ rule(id="10.2:8", cat="normative") }}
The `@link_name("…")` directive overrides the linker symbol name of an extern declaration. Without `@link_name`, the symbol equals the Gruel identifier.

{{ rule(id="10.2:9", cat="legality-rule") }}
Use of `link_extern` requires the `c_ffi` preview feature.

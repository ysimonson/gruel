+++
title = "@mark(c) marker"
weight = 1
template = "spec/page.html"
+++

# `@mark(c)` marker (ADR-0085)

{{ rule(id="10.1:1", cat="normative") }}
The `@mark(c)` marker applies to function and struct declarations. Applying it to an enum is a compile-time error in v1; future ADRs may lift the restriction once a `c_int`-shaped type is available.

{{ rule(id="10.1:2", cat="normative") }}
On a function declaration, `@mark(c)` selects the platform C calling convention and suppresses Gruel name mangling on the emitted symbol. Such a function is callable from C using its Gruel identifier as the symbol name unless overridden by `@link_name("…")`.

{{ rule(id="10.1:3", cat="normative") }}
On a struct declaration, `@mark(c)` selects C-compatible field layout: field order matches declaration order, each field is placed at the lowest offset satisfying its natural alignment, and niche optimisation is disabled.

{{ rule(id="10.1:4", cat="example") }}
```gruel
@mark(c) fn my_callback(x: i32) -> i32 { x + 1 }

@mark(c) struct Point { x: i32, y: i32 }
```

{{ rule(id="10.1:5", cat="legality-rule") }}
Use of `@mark(c)` requires the `c_ffi` preview feature.

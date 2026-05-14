+++
title = "@mark(c) marker"
weight = 1
template = "spec/page.html"
+++

# `@mark(c)` marker (ADR-0085)

{{ rule(id="10.1:1", cat="normative") }}
The `@mark(c)` marker applies to function, struct, and enum declarations. (ADR-0086 widened the applicability set to include enums; the enum case is documented in section [10.4](@/10-c-ffi/04-enum-ffi.md).)

{{ rule(id="10.1:2", cat="normative") }}
On a function declaration, `@mark(c)` selects the platform C calling convention and suppresses Gruel name mangling on the emitted symbol. Such a function is callable from C using its Gruel identifier as the symbol name unless overridden by `@link_name("…")`.

{{ rule(id="10.1:3", cat="normative") }}
On a struct declaration, `@mark(c)` selects C-compatible field layout: field order matches declaration order, each field is placed at the lowest offset satisfying its natural alignment, and niche optimisation is disabled.

{{ rule(id="10.1:4", cat="example") }}
```gruel
@mark(c) fn my_callback(x: i32) -> i32 { x + 1 }

@mark(c) struct Point { x: i32, y: i32 }
```

{{ rule(id="10.1:5", cat="normative") }}
A `@mark(c)` struct may not declare an inline `fn __drop` destructor: cross-boundary destructor semantics are not defined in this revision.

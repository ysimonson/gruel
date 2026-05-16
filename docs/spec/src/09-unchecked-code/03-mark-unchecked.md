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

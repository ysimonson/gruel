+++
title = "Anonymous Functions"
weight = 16
template = "spec/page.html"
+++

# Anonymous Functions

Anonymous functions are expressions that denote a callable value (ADR-0055).

## Syntax

{{ rule(id="4.16:1", cat="syntax") }}

An anonymous function expression has the form `fn(params) { body }`,
`fn(params) -> ret { body }`, or `fn() { body }` / `fn() -> ret { body }`. It
is syntactically identical to a named function item with the name omitted.

{{ rule(id="4.16:2", cat="syntax") }}

Each parameter **MUST** carry a type annotation, as with named functions. The
return-type clause `-> T` is optional; when omitted, the return type defaults
to `()`.

## Desugaring

{{ rule(id="4.16:3", cat="normative") }}

An anonymous function expression is semantically equivalent to a struct
expression of a fresh anonymous struct type with no fields and one method
named `__call`, whose signature and body match the anonymous function's
parameters and body. The expression evaluates to an empty instance of that
struct.

{{ rule(id="4.16:4") }}

```gruel
fn main() -> i32 {
    let f = fn(x: i32) -> i32 { x + 1 };
    // Conceptually equivalent to constructing an instance of:
    //   struct { fn __call(self, x: i32) -> i32 { x + 1 } }
    f(41)
}
```

## Type uniqueness

{{ rule(id="4.16:5", cat="normative") }}

Each anonymous function expression in source produces a distinct type. Two
anonymous function expressions with identical signatures but different bodies
**MUST** be treated as values of different types. This rule differs from the
structural equality rule for anonymous struct types (§3.9), so that the
compiler can always determine which body to call.

## Call-sugar

{{ rule(id="4.16:6", cat="normative") }}

A function call expression `f(args)` whose callee `f` resolves to a local
binding or parameter of type `T`, where `T` is a struct type with a method
named `__call`, is equivalent to the method call `f.__call(args)`. This sugar
applies to any struct with a `__call` method, not only anonymous functions.

## Capture

{{ rule(id="4.16:7", cat="normative") }}

The body of an anonymous function may reference comptime parameters of the
enclosing function, module-level items, and names introduced inside the
anonymous function itself (its parameters or nested bindings).

{{ rule(id="4.16:8", cat="legality-rule") }}

The body of an anonymous function **MUST NOT** reference a runtime local
(`let` binding or runtime parameter) of an enclosing function. Doing so is a
compile-time error. To pass runtime values, provide them as explicit
arguments or define a named struct with a `__call` method whose fields hold
the captured state.

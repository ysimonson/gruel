+++
title = "Tuple Expressions"
weight = 15
template = "spec/page.html"
+++

# Tuple Expressions

## Tuple Literals

{{ rule(id="4.15:1", cat="normative") }}

A tuple literal constructs a tuple value from a list of element expressions. It is written as a parenthesised, comma-separated list: `(e0, e1, ..., eN-1)`.

{{ rule(id="4.15:2", cat="syntax") }}

A 1-element tuple literal **MUST** use a trailing comma: `(e,)`. The form `(e)` (no trailing comma) is a parenthesised expression and is not a tuple.

{{ rule(id="4.15:3", cat="normative") }}

The expression `()` has unit type, not tuple type. The expression `(e)` has the same type as `e` (parenthesised expression).

{{ rule(id="4.15:4", cat="normative") }}

The type of a tuple literal `(e0, e1, ..., eN-1)` is `(T0, T1, ..., TN-1)` where each `Ti` is the type of `ei`.

{{ rule(id="4.15:5", cat="dynamic-semantics") }}

Elements are evaluated in left-to-right (source) order. The resulting tuple is then constructed from the evaluated elements.

{{ rule(id="4.15:6") }}

```gruel
fn main() -> i32 {
    let p: (i32, bool) = (1, true);
    let singleton: (i32,) = (42,);
    p.0
}
```

## Tuple Field Access

{{ rule(id="4.15:7", cat="normative") }}

A tuple element is accessed with the expression `t.N`, where `N` is a non-negative integer literal in decimal form.

{{ rule(id="4.15:8", cat="legality-rule") }}

The index `N` **MUST** be strictly less than the arity of `t`. Indexing out of bounds is a compile-time error.

{{ rule(id="4.15:9") }}

Tuple indices are written as decimal integer literals. The forms `t.0x1` (hexadecimal) and `t.1e10` (exponent syntax) are not tuple indices; the former fails to parse as field access and the latter is tokenised as a single float literal (see 4.15:10).

{{ rule(id="4.15:10", cat="normative") }}

Nested tuple access of the form `t.0.1` is not currently supported because the lexer tokenises `0.1` as a single float literal. Write `(t.0).1` to read the `1` element of `t.0`.

{{ rule(id="4.15:11", cat="legality-rule") }}

If the element type at index `N` is not `Copy`, the expression `t.N` **MUST NOT** be used to consume that element. Such an expression is a compile-time error; the programmer **MUST** use tuple destructuring (see [Let Statements](../../05-statements)) to consume non-copy elements.

{{ rule(id="4.15:12") }}

```gruel
fn main() -> i32 {
    let p = (10, 20);
    p.0 + p.1  // 30
}
```

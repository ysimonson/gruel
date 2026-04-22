+++
title = "Tuple Types"
weight = 12
template = "spec/page.html"
+++

# Tuple Types

{{ rule(id="3.12:1", cat="normative") }}

A tuple type is a fixed-arity, heterogeneous product of element types. It is written as a parenthesised, comma-separated list of types. A tuple type with N elements has the form `(T0, T1, ..., TN-1)`.

{{ rule(id="3.12:2", cat="syntax") }}

A 1-tuple type **MUST** be written with a trailing comma: `(T,)`. The form `(T)` (without a trailing comma) is a parenthesised type and is not a tuple.

{{ rule(id="3.12:3", cat="normative") }}

The zero-arity tuple type `()` is the unit type (see [Unit Type](../03-unit-type)). The unit type and the empty tuple are the same type.

{{ rule(id="3.12:4", cat="normative") }}

Two tuple types are the same type if and only if they have the same arity and their element types are the same in order.

{{ rule(id="3.12:5", cat="normative") }}

A tuple type is `@copy` if and only if every element type is `@copy`. Otherwise the tuple is moved on use, subject to the same affine-type rules as other non-copy aggregates.

{{ rule(id="3.12:6", cat="normative") }}

An element of a tuple value `t` is accessed with the expression `t.N`, where `N` is a non-negative integer literal in decimal form (see [Tuple Expressions](../../04-expressions)). Indexing out of bounds (`N >= arity`) is a compile-time error.

{{ rule(id="3.12:7", cat="legality-rule") }}

If a tuple element type is not `@copy`, the element **MUST NOT** be consumed via field access (`t.N`). To consume individual elements, the tuple **MUST** be destructured (see [Let Statements](../../05-statements)).

{{ rule(id="3.12:8") }}

```gruel
let p: (i32, bool) = (42, true);  // 2-tuple
let one: (i32,) = (42,);           // 1-tuple (trailing comma required)
let u: () = ();                    // unit type
let nested: ((i32, i32), bool) = ((1, 2), false);
```

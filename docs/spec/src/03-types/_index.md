+++
title = "Types"
weight = 3
sort_by = "weight"
template = "spec/section.html"
page_template = "spec/page.html"
+++

# Types

This chapter describes the type system of Gruel.

{{ rule(id="3.0:1") }}

Every value in Gruel has a type that determines its representation in memory and the operations that can be performed on it.

## Zero-Sized Types

{{ rule(id="3.0:2", cat="normative") }}

A zero-sized type (ZST) is a type with a size of zero bytes. Zero-sized types can be instantiated and passed by value, but they occupy no storage.

{{ rule(id="3.0:3", cat="normative") }}

The following types are zero-sized:
- The unit type `()`
- The never type `!`
- Empty structs (structs with no fields)
- Zero-length arrays `[T; 0]` for any type `T`

{{ rule(id="3.0:4", cat="normative") }}

Zero-sized types have an alignment of 1 byte.

## Layout Guarantees

{{ rule(id="3.0:5", cat="normative") }}

Every type in Gruel has a defined size (in bytes) and alignment, observable
via the `@size_of(T)` and `@align_of(T)` intrinsics. The size and alignment
of any given type are stable: two compilations of the same program with the
same compiler version, target, and feature set yield the same numbers.

{{ rule(id="3.0:6", cat="normative") }}

The specific in-memory representation of a value — including the presence,
position, and width of any internal discriminant — is an implementation
choice except where this specification explicitly guarantees otherwise.
Implementations may use alternative encodings such as niche-filling for
enums whose payload type forbids certain bit patterns. The `@size_of`
result for a type may shrink under such optimizations.

{{ rule(id="3.0:7", cat="normative") }}

The only portable observables of a value are: pattern matching, equality
(`==` / `!=`), field access on structs, payload-binding pattern matches on
enums, indexing on arrays, and the `@size_of` / `@align_of` intrinsics.
Programs that interpret the raw bytes of a value (for example, via
`@transmute`) depend on the implementation-defined representation and are
not guaranteed to be portable across compiler versions.

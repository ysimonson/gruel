+++
title = "Slices"
weight = 2
template = "spec/page.html"
+++

# Slices

Slices are scope-bound, non-owning views over a contiguous run of values
of the same type. See ADR-0064.

This chapter is **incomplete** — it is filled in as the slices preview
feature is implemented. Until ADR-0064 is stabilized, slice surface forms
require `--preview slices`.

## Types

{{ rule(id="7.2:1", cat="normative") }}

A *slice type* is one of `Slice(T)` (immutable view) or `MutSlice(T)`
(mutable view), where `T` is any non-comptime element type.

{{ rule(id="7.2:2", cat="normative") }}

A slice value is a *fat pointer* `{ ptr, len }` consisting of a pointer
to the first element and a length in elements.

## Range Subscripts

{{ rule(id="7.2:3", cat="informative") }}

```ebnf
range  = expression ".." expression                  (* a..b *)
       | expression ".."                             (* a..  *)
       | ".." expression                             (* ..b  *)
       | ".."                                        (* ..   *)
       ;

subscript = "[" ( expression | range ) "]" ;
```

{{ rule(id="7.2:4", cat="informative") }}

Ranges are recognized **only** in subscript position. They are not yet a
general-purpose expression form: `let r = 0..10;` and `for i in 0..n`
are not valid uses of a bare range expression.

{{ rule(id="7.2:5", cat="normative") }}

A range subscript `arr[lo..hi]` is a *place expression* naming a
sub-place of `arr`. The endpoints are half-open: the resulting view
covers indices `[lo, hi)`. When `lo` is omitted it defaults to `0`;
when `hi` is omitted it defaults to the array length.

{{ rule(id="7.2:6", cat="legality-rule") }}

For a range subscript on an array of length `N`, the program **MUST**
satisfy `lo <= hi <= N`. When both endpoints are constant the check is
performed at compile time; otherwise it is performed at runtime.

{{ rule(id="7.2:7", cat="dynamic-semantics") }}

When `lo > hi` or `hi > N` at runtime, the program panics.

## Slice Construction via Borrow

{{ rule(id="7.2:8", cat="normative") }}

`&arr[range]` produces a `Slice(T)` view of the indexed sub-range.
`&mut arr[range]` produces a `MutSlice(T)` view; the receiver **MUST**
be a mutable place.

{{ rule(id="7.2:9", cat="legality-rule") }}

Range subscripts are valid only as the place under `&` / `&mut`. A range
subscript used as an rvalue (e.g. `let s = arr[1..3];`) is rejected;
there is no slice value without a borrow.

## Indexing

{{ rule(id="7.2:10", cat="normative") }}

For a slice `s` and index `i: usize`, the expression `s[i]` evaluates to
the element at position `i`.

{{ rule(id="7.2:11", cat="dynamic-semantics") }}

When `i >= s.len()` at runtime, `s[i]` causes the program to panic.

{{ rule(id="7.2:12", cat="legality-rule") }}

`s[i]` for a slice whose element type is not `Copy` is rejected — it
would move out of indexed position. (Mirrors the array rule from
7.1:28.)

{{ rule(id="7.2:13", cat="normative") }}

`s[i] = v` is an assignment to the element at position `i`. It is valid
only when `s` has type `MutSlice(T)`. Bounds-check semantics follow
7.2:11.

## `checked`-block Operations

{{ rule(id="7.2:14", cat="normative") }}

The methods `s.ptr()` (on any slice) and `s.ptr_mut()` (on `MutSlice(T)`
only) extract the underlying data pointer. Both **MUST** appear inside a
`checked` block.

{{ rule(id="7.2:15", cat="normative") }}

The intrinsics `@parts_to_slice(p, n)` and `@parts_to_mut_slice(p, n)`
build a slice from a raw pointer and a length. They **MUST** appear
inside a `checked` block. `@parts_to_slice` accepts `Ptr(T)` and
produces `Slice(T)`; `@parts_to_mut_slice` accepts `MutPtr(T)` and
produces `MutSlice(T)`.

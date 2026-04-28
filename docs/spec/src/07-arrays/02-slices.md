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

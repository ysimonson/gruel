+++
title = "Vectors"
weight = 3
+++

# Vectors

This section documents `Vec(T)` — the language's owned, growable
heap-allocated vector — per ADR-0066.

## Type Form

{{ rule(id="7.3:1", cat="normative") }}

`Vec(T)` is a built-in parameterized type constructor that lowers to
`TypeKind::Vec(VecTypeId)` internally. The runtime representation is the
3-field aggregate `{ ptr: *T, len: i64, cap: i64 }` (24 bytes on 64-bit
targets, 8-byte aligned). `Vec(T)` is affine — it owns heap-allocated
storage that the compiler-generated drop releases when the value goes
out of scope.

`Vec(T)` is gated behind the `vec` preview feature; using the name in
type position without `--preview vec` is rejected. Element types **MUST
NOT** be `linear`; the compiler rejects `Vec(T:Linear)` at type-resolution
time.

The construction methods (`Vec::new`, `Vec::with_capacity`) and the rest
of the method surface (length queries, `push` / `pop`, indexing, slice
borrowing, iteration, `Clone`, FFI helpers) are documented alongside
their phases as ADR-0066 lands.

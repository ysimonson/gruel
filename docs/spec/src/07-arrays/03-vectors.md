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

## Construction

{{ rule(id="7.3:2", cat="normative") }}

Two associated functions construct a `Vec(T)`:

- `Vec(T)::new() -> Vec(T)` — empty vector, `cap = 0`, `ptr = null`.
- `Vec(T)::with_capacity(n: usize) -> Vec(T)` — empty vector with
  `cap >= n`. Allocates iff `n > 0`.

The variadic `@vec(a, b, c, ...)` intrinsic builds a `Vec(T)` from its
arguments: `cap = len = arg_count`, each argument moved into a slot.
At least one argument is required; element types unify to a single
`T`. The element type **MUST NOT** be `linear`.

The `@vec_repeat(v: T, n: usize) -> Vec(T)` intrinsic builds a vector
with `n` copies of `v`. v1 requires `T: Copy`; non-Copy `T: Clone`
support is future work alongside the Clone synthesis path.

## Methods

{{ rule(id="7.3:3", cat="normative") }}

The instance method surface:

- `len(borrow self) -> usize` / `capacity(borrow self) -> usize` /
  `is_empty(borrow self) -> bool` — runtime field reads.
- `push(inout self, value: T) -> ()` — append `value`, growing the
  buffer (doubling, min cap 4) on `len == cap`.
- `pop(inout self) -> T` — panic if empty, else remove and return
  the last element. (v1 returns `T` directly; future work to wrap
  in `Option(T)`.)
- `clear(inout self) -> ()` — drop each live element if `T` needs
  drop, then set `len = 0`. Capacity is preserved.
- `reserve(inout self, additional: usize) -> ()` — ensure
  `cap >= len + additional`.
- `clone(borrow self) -> Vec(T)` — deep copy. v1: requires
  `T: Copy` (memcpy path).

Indexing: `v[i]` reads bounds-checked at runtime, requires `T: Copy`.
`v[i] = x` writes bounds-checked at runtime.

## Slice borrowing

{{ rule(id="7.3:4", cat="normative") }}

Range subscripts on a `Vec(T)` produce `Slice(T)` / `MutSlice(T)`
borrows over the live `[0..len]` range, exactly as for fixed arrays.
`&v[..]` / `&mut v[a..b]` use the runtime `len` field for bounds
checking; the buffer pointer comes from the `ptr` field.

## Iteration

{{ rule(id="7.3:5", cat="normative") }}

`for x in v` over `Vec(T)` desugars to `for x in &v[..]`: a borrowed
slice view that yields each element by value (for `Copy` `T`). The
mutable form is deferred to a future ADR alongside the equivalent
slice-mut iteration.

## FFI / `checked` block

{{ rule(id="7.3:6", cat="normative") }}

Inside a `checked` block:

- `v.ptr() -> Ptr(T)` — read-only pointer to the buffer.
- `v.ptr_mut() -> MutPtr(T)` — mutable pointer to the buffer.
- `v.terminated_ptr(s: T) -> Ptr(T)` — ensure `cap > len`, write `s`
  at `ptr[len]`, return the buffer pointer. The sentinel byte sits
  outside the live `[0..len]` range and is overwritten by the next
  `push`.
- `@parts_to_vec(p: MutPtr(T), len: usize, cap: usize) -> Vec(T)` —
  take ownership of an existing buffer.

## Drop

{{ rule(id="7.3:7", cat="dynamic-semantics") }}

Dropping a `Vec(T)` runs the per-element drop loop (when `T` needs
drop) over `[0..len]`, then frees the buffer if `cap > 0`. The element
drop dispatches via the same `__gruel_drop_*` machinery used for
ordinary affine struct fields.

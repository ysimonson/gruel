---
id: 0066
title: Vec(T) — Owned, Growable Vector with On-Demand Sentinel
status: implemented
tags: [types, generics, collections, heap, ffi]
feature-flag: vec
created: 2026-04-30
accepted: 2026-05-01
implemented: 2026-05-01
spec-sections: ["7.3"]
superseded-by:
---

# ADR-0066: Vec(T) — Owned, Growable Vector with On-Demand Sentinel

## Status

Implemented (with two v1 limitations carried as future-work — see "v1 limitations" below).

## Summary

Introduce `Vec(T)` as the language's owned, heap-allocated, growable vector — a fat pointer carrying an element pointer, length, and capacity (`{ ptr, len, cap }`, 24 bytes, identical to Rust's `Vec<T>`). `Vec(T)` is a built-in *generic* type constructor (parallel to `Slice(T)` from ADR-0064) lowered through monomorphization: per-`T` codegen produces inline LLVM for `push`, `pop`, indexing, drop, etc., calling into existing allocator primitives in `gruel-runtime` for grow/free. Sentinel termination — the substrate for null-terminated FFI buffers and (eventually) string types — is handled *on demand* at the FFI boundary via a `terminated_ptr(sentinel: T) -> Ptr(T)` method that ensures `cap >= len + 1`, writes the sentinel at `ptr[len]`, and returns the pointer. The sentinel is not stored, not flagged, not maintained across mutations; it's re-established at each handoff. This keeps `Vec(T)` zero-overhead in the common case (no per-Vec storage tax, no per-mutation branch) and makes FFI handoff explicit at every call site. Slice borrowing (`&v[..]`, `&mut v[a..b]`) integrates with ADR-0064; `pop` returns `Option(T)` and `clone` is constrained on `Clone`, both from ADR-0065. A `@vec(a, b, c)` variadic intrinsic provides Rust-style literal construction; `@vec_repeat(v, n)` covers the repetition pattern. The existing monomorphic `String` type is left untouched; a future ADR can migrate it to `Vec(u8)` if desired.

## Context

Gruel today has:

- **Fixed-size arrays** (`[T; N]`, spec ch. 7) — stack-allocated, length baked into the type.
- **Slices** (`Slice(T)` / `MutSlice(T)`, ADR-0064) — non-owning views over contiguous storage; scope-bound, no escape.
- **`String`** (ADR-0020 / `gruel-builtins::STRING_TYPE`) — a monomorphic owned heap type with layout `{ ptr, len, cap }` and a fixed set of byte-oriented methods. Its runtime functions are FFI-style: `String__push`, `String__concat`, etc., one per method, returning the new struct via sret.
- **No way to write `Vec<T>` for arbitrary `T`.** User code that wants a heap-backed list of `i32`s or `MyStruct`s has no first-class option. The only escape hatches are (a) raw `MutPtr(T)` plus manual length tracking inside `checked` blocks, or (b) defining a `String`-shaped type by hand for each `T`.
- **No FFI substrate for null-terminated buffers.** `String` is *not* null-terminated. Slices' construction-time sentinels (ADR-0064) handle the read-only case but cannot survive mutation.

### What's missing, concretely

1. **Generic owned heap container.** Every nontrivial program eventually wants a growable list. Forcing users to roll their own per-`T` is a tax we shouldn't levy.
2. **Mechanism for built-in *generic* heap types.** The `gruel-builtins` synthetic-struct mechanism is monomorphic — it works for `String` but not for `Vec(T)`. The closest existing generic mechanism is the `Slice(T)` machinery in `gruel-air` / `gruel-codegen-llvm`, which lowers per-method-per-`T` inline rather than dispatching to a single runtime FFI function. `Vec(T)` should follow that model, not the `String` one.
3. **A way to hand a Vec to C as a NUL-terminated buffer.** C-string FFI, NUL-terminated buffer hand-off to syscalls, and similar use cases need a sentinel byte at `ptr[len]`. The question is *where* the sentinel obligation lives. Three positions are defensible:
   - **In the type** (`Vec(T, :s)`, Zig-style). Requires comptime-values-in-type-arguments machinery.
   - **In the runtime data** (a `sentinel` field plus a `has_sentinel` flag, maintained across mutations). Pays per-Vec storage and per-mutation-branch overhead.
   - **At the boundary** (the sentinel is established only when `terminated_ptr(s)` is called). Zero per-Vec cost; FFI call sites name the sentinel explicitly.
   
   This ADR takes the third path. Reasoning: the FFI handoff is the *only* place where the sentinel matters, and the cost of re-establishing it on demand (one capacity check + one byte write per handoff) is negligible compared to the cost of a foreign function call. Building the sentinel into the layout or the type system pays a recurring tax for a moment-of-handoff property — the wrong shape of cost.

### What this ADR does *not* attempt

- **Replace or migrate `String`.** `String` stays monomorphic and unchanged. A future ADR may migrate `String` to a `Vec(u8)` substrate with a UTF-8 invariant; the design here is deliberately compatible with that direction without committing to it.
- **Allocator parameterization.** ADR-0064's "future work" notes a `Vec(T, A)` over an allocator interface. This ADR uses the global allocator only. Adding allocators is a separate ADR once an `Allocator` interface lands.
- **Type-tracked sentinel values.** No `Vec(T, :s)` form. The sentinel only ever appears as an argument to `terminated_ptr`. Promoting the sentinel into the type would require comptime-values-in-types machinery; out of scope.
- **Persistent / maintained sentinel invariant.** The sentinel is *not* maintained across `push` / `pop` / `clear` / `reserve`. Each FFI handoff calls `terminated_ptr(s)` afresh.
- **Linear element types.** `Vec(T)` for `T: Linear` is rejected at sema time. See "Linear elements" below.
- **`String`-like rich method surface.** Methods are minimal-but-useful (see "Methods" below). `extend`, `insert`, `remove`, `swap_remove`, `truncate`, `drain`, `dedup`, `sort`, etc. are deliberate omissions — easy follow-ups once the registry is in place.

### Where Gruel sits relative to other languages

- **Rust** monomorphizes `Vec<T>`. Methods are inlined per `T`. Allocator is parameterized via `A: Allocator`. No sentinel; null-terminated strings are a separate `CString` type, constructed via `CString::from_vec_with_nul_unchecked` (the user appends the NUL and converts).
- **Zig** has `std.ArrayList(T)` (heap-backed, growable) with no sentinel support. The idiom for null-terminated growable strings is `ArrayList(u8)` + `toOwnedSliceSentinel(0)`, which appends the sentinel at the conversion boundary. Type-level sentinels (`[N:s]T`, `[:s]T`) exist for fixed arrays and slices but not for the growable container.
- **C++** has `std::vector<T>` (monomorphized via templates). `std::string` does carry a NUL terminator at `data()[size()]` as a maintained invariant since C++11 — but `std::string` is a single concrete type, not a generic vector.

Gruel takes the Rust monomorphization path for the owned-vector itself, and (informed by the Zig idiom) handles sentinel termination as a *boundary conversion* rather than a maintained invariant. The difference from Rust is that we don't introduce a separate `CString` type — the conversion is a method on `Vec(T)` that returns a `Ptr(T)` directly.

## Decision

### The type

A new built-in parameterized type constructor:

- `Vec(T)` — owned, heap-allocated, growable vector of `T`.

Internally: `TypeKind::Vec(TypeId)`, interned in the type pool like `TypeKind::Slice` / `TypeKind::PtrConst`. Registered in `BUILTIN_TYPE_CONSTRUCTORS` as `BuiltinTypeConstructorKind::Vec`, parallel to `Slice` / `MutSlice`.

### Layout

For a given `T`, the LLVM lowering of `Vec(T)` is the aggregate:

```
{ ptr: T*,  len: i64,  cap: i64 }
```

24 bytes, identical to Rust's `Vec<T>` modulo allocator parameterization.

- `ptr` — heap allocation containing `cap` `T` slots, or null when `cap == 0`.
- `len` — number of valid initialized elements (always `<= cap`).
- `cap` — number of `T` slots in the allocation. Zero means no allocation.

The single layout invariant is `cap >= len`. There is no sentinel field, no termination flag, no special "sentinel-active" regime.

### Move semantics

`Vec(T)` is **affine** (not `Copy`), like `String`. Moving is a bitwise copy of the three-field aggregate. Drop is generated automatically by the compiler.

`Vec(T)` is fully **owned** and **storable**: it can be returned from functions, stored in struct fields, captured by closures, etc. (Unlike `Slice(T)` from ADR-0064, which is scope-bound.)

### Methods (v1 surface)

The method set is registered in a new `VEC_METHODS` table in `gruel-intrinsics`, parallel to `SLICE_METHODS` (ADR-0064) and `POINTER_METHODS` (ADR-0063). Each entry names the method and its codegen lowering — there is no per-method runtime FFI function; codegen emits inline LLVM that calls allocator primitives only.

#### Construction (associated functions)

| Form | Signature | Notes |
|------|-----------|-------|
| `Vec::new()` | `() -> Vec(T)` | empty, `cap = 0`, `ptr = null` |
| `Vec::with_capacity(n)` | `(n: usize) -> Vec(T)` | empty, `cap >= n`, allocation only if `n > 0` |

Both return by value (sret on the codegen side). Standard Rust semantics.

#### Literal construction — `@vec`

A variadic intrinsic for inline literal construction, mirroring Rust's `vec![…]`:

```gruel
let v: Vec(i32) = @vec(1, 2, 3);          // len=3, cap=3
let s: Vec(String) = @vec(s1, s2);        // s1, s2 moved in
```

Semantics: `@vec(a1, ..., an)` requires `n >= 1`, type-unifies all arguments to a single `T`, allocates a buffer with `cap = n`, moves each argument into `ptr[i]`, and returns a `Vec(T)` with `len = n`. Standard move semantics for affine arguments.

The empty case (`@vec()`) is **not** supported in v1 — it would require bidirectional inference of `T` from the LHS type ascription. Use `Vec::new()` for empty construction. Lifting this restriction is a follow-up if ergonomics demand it.

Linearity: rejected for `T: Linear` consistent with all `Vec(T:Linear)` rejection in this ADR.

`@vec` is registered in the existing `INTRINSICS` registry in `gruel-intrinsics` as `IntrinsicId::Vec`, gated to `--preview vec`. Variadic dispatch follows the existing `@dbg` precedent.

#### Repetition construction — `@vec_repeat`

A second intrinsic for the "n copies of the same value" pattern (Rust's `vec![v; n]`):

```gruel
let zeros: Vec(i32) = @vec_repeat(0, 100);     // 100 zeros
let blanks: Vec(String) = @vec_repeat(empty, 5); // five clones of `empty`
```

Semantics: `@vec_repeat(v: T, n: usize)` where `T: Clone` allocates `cap = n` slots and fills them with copies of `v`. Codegen:

- For `n == 0`: return empty `Vec(T)`. `v` is consumed (dropped via standard drop machinery).
- For `n >= 1`: clone `v` into `ptr[0..n-1]` (that's `n-1` clone calls), then move `v` into `ptr[n-1]` to avoid one extra clone. `len = n`.
- For `T: Copy`: the clone calls collapse to a `memcpy` (or an unrolled-store loop for tiny `n`); `v` is bitwise-duplicated into every slot, with no special handling for the last slot.

The constraint `T: Clone` is what makes this distinct from `@vec` — `@vec` only moves args, `@vec_repeat` duplicates one arg, so it needs the Clone interface from ADR-0065. Since all `Copy` types automatically conform to `Clone`, the `T: Copy` ergonomics aren't affected.

`@vec_repeat` is registered as `IntrinsicId::VecRepeat`, gated to `--preview vec`. Linearity rejection follows the same rule as everything else in this ADR.

Why a separate intrinsic instead of `@vec(v; n)` syntax? A semicolon-inside-call-args parser change would be a one-off grammar quirk; a second intrinsic is cleaner and consistent with the existing flat-call shape.

#### Queries (`&self`)

| Method | Signature |
|--------|-----------|
| `len` | `(&self) -> usize` |
| `capacity` | `(&self) -> usize` |
| `is_empty` | `(&self) -> bool` |

#### Indexing

| Form | Receiver | Signature | Constraint |
|------|----------|-----------|------------|
| `v[i]` (read) | `Vec(T)`, `&Vec(T)`, `&mut Vec(T)` | `(self, i: usize) -> T` | `T: Copy`; runtime bounds check |
| `v[i] = val` (write) | `&mut Vec(T)` | `(self, i: usize, v: T) -> ()` | runtime bounds check |

Read-of-non-Copy is rejected, mirroring spec 7.1:28 and ADR-0064. Future `take`/`swap` methods can lift this.

#### Mutation (`&mut self`)

| Method | Signature | Notes |
|--------|-----------|-------|
| `push` | `(&mut self, value: T) -> ()` | grow if `len == cap`; write `value` at `ptr[len]`; `len += 1` |
| `pop` | `(&mut self) -> Option(T)` | empty: returns `None`; otherwise `len -= 1`, moves out of `ptr[len]`, returns `Some(...)` |
| `clear` | `(&mut self) -> ()` | drop all elements `[0..len]`; `len = 0`; capacity unchanged |
| `reserve` | `(&mut self, additional: usize) -> ()` | ensure `cap >= len + additional` |

`pop` returns `Option(T)` — `None` on an empty Vec, `Some(t)` otherwise (a move out of the last slot, leaving `len` decremented behind it). Same shape as `Vec::pop` in Rust. `Option(T)` comes from ADR-0065.

These methods are pure Rust-Vec semantics. No sentinel maintenance, no extra branches.

#### Cloning

| Method | Signature | Constraint |
|--------|-----------|------------|
| `clone` | `(&self) -> Vec(T)` | `T: Clone` |

`Clone` is the structural interface defined by ADR-0065. All `Copy` types automatically conform; affine types (e.g. `String`, `Vec(_)`) opt in via `@derive(Clone)` or a hand-written `clone` method. Codegen for `Vec(T).clone()` allocates `cap` slots, then for each `i in 0..len` writes `self.ptr[i].clone()` to `new.ptr[i]`. For `T: Copy` this collapses to a single `memcpy(len * sizeof(T))`; for non-Copy `T: Clone` it's a per-element clone loop.

`Vec(T)` itself is `Clone` when `T: Clone` — that conformance is what makes `Vec(Vec(i32))` etc. clone naturally.

#### FFI / unchecked (`checked` block)

| Form | Signature | Notes |
|------|-----------|-------|
| `v.ptr()` | `(&self) -> Ptr(T)` | raw element pointer; like `Slice::ptr()` from ADR-0064 |
| `v.ptr_mut()` | `(&mut self) -> MutPtr(T)` | mutable raw pointer |
| `v.terminated_ptr(s: T)` | `(&mut self, s: T) -> Ptr(T)` | the on-demand sentinel; see below |
| `@parts_to_vec(p, len, cap)` | `(p: MutPtr(T), len: usize, cap: usize) -> Vec(T)` | takes ownership of `p` |

The `@parts_to_vec` intrinsic is added to the existing `INTRINSICS` registry in `gruel-intrinsics`, gated to `checked` blocks like ADR-0064's `@parts_to_slice`.

### On-demand sentinel termination

`terminated_ptr` is the linchpin of the sentinel design. Its semantics:

```text
fn terminated_ptr(&mut self, s: T) -> Ptr(T)   where T: Copy
    // 1. Ensure cap > len. If cap == len, grow (doubling).
    // 2. Write s into ptr[len]. (Does NOT increment len.)
    // 3. Return ptr (cast to Ptr(T)).
```

After the call:

- `ptr[len] == s` until the next mutation overwrites it.
- `len` is unchanged. The sentinel sits at index `len`, which is *outside* the live element range.
- `cap >= len + 1`.
- The returned `Ptr(T)` is valid until the Vec is mutated or dropped.

The method is `&mut self` because it may grow the buffer. Pulling a terminated pointer from a `&Vec(T)` is not supported; the borrow-checker enforces exclusive access for the duration of the call.

The constraint `T: Copy` exists because writing a non-Copy `T` into `ptr[len]` would move the sentinel into a slot that the drop loop never visits (drop walks `[0..len]`, not `[0..len+1]`), leaking the sentinel's destructor. FFI sentinels are always primitives in practice, so this is a tight fit.

#### What "on-demand" buys

- **Zero per-Vec memory tax.** Layout stays `{ ptr, len, cap }`.
- **Zero per-mutation branch.** `push`/`pop`/`clear`/`reserve` are pure Rust-Vec.
- **Explicit at the call site.** Every FFI handoff names the sentinel value visibly.
- **Re-establishment is cheap.** O(1) per FFI call: at most one realloc-by-doubling (amortized O(1)) and one byte write.

#### What "on-demand" gives up

- **The sentinel does not survive across mutations.** After `let p = v.terminated_ptr(0); call_c(p); v.push(x);`, the byte that was the NUL is now `x`. The next FFI call needs another `terminated_ptr(0)`. *Correct* — the sentinel is a momentary FFI-boundary contract, not an ongoing property — but it's a behavioral difference worth documenting.
- **Pointer-invalidation hazard.** `let p = v.terminated_ptr(0); v.push(x); call_c(p);` — if the push reallocated, `p` is dangling. Same hazard as Rust's `Vec::as_ptr` followed by `push`, and the same answer: raw pointers are `checked`-block escapes (ADR-0028); the borrow-checker enforces exclusive `&mut` access during the `terminated_ptr` call but does not track the lifetime of the returned raw pointer. User responsibility.

### Slice borrowing

`Vec(T)` integrates with ADR-0064's place-grammar extension. Range subscripts on a Vec produce slices, exactly as for arrays:

```gruel
let v: Vec(i32) = Vec::with_capacity(4);
// ...push some elements...
let s: Slice(i32)    = &v[..];        // view of all len elements
let m: MutSlice(i32) = &mut v[..];    // mut view; requires `let mut v`
let mid: Slice(i32)  = &v[1..3];      // sub-range
```

Crucially, slice borrows view the **live `len` elements**, not the `cap` allocation. A `Slice(T)` of `&v[..]` has length `v.len()`. Because there is no maintained sentinel byte, slicing has nothing to exclude.

The borrow-checker treatment is the same as for arrays: `&v[..]` borrows the whole Vec for the slice's scope. Mutating methods (`push`, `pop`, `terminated_ptr`, etc.) cannot run while a slice borrow is live. Split-borrows are future work (same as ADR-0064).

Range subscripts on a Vec rvalue (`let r = v[1..3]`) are rejected — slice values exist only as borrows, exactly as in ADR-0064.

### Iteration

`Vec(T)` integrates with ADR-0041 for-each loops:

```gruel
for x in v {                    // x: T, requires T: Copy (v1)
    total = total + x;
}

for x in &v {                   // x: T (Copy) or future Ref(T)
    ...
}

for x in &mut v {               // x: MutRef(T) (depends on ADR-0062 phase 8)
    ...
}
```

For-each over `Vec(T)` lowers to for-each over `&v[..]` — i.e. the iteration is on the slice view, not directly on the Vec. This means iteration semantics, Copy-vs-non-Copy rules, and mut-iteration deps all inherit from ADR-0064 phase 8 verbatim. The mut form is gated on the same deref-assignment operator that ADR-0062 / ADR-0064 phase 8 calls out.

### Linear elements

`Vec(T)` is rejected at sema time when `T: Linear` (per ADR-0008's ownership ladder: linear values must be explicitly consumed; implicit drop is a compile error).

The reason: Vec runs an implicit drop loop over `[0..len]` when it itself is dropped, when `clear` is called, and when an indexed write replaces an existing element. Each of those is an *implicit* drop of the elements, which violates linearity. There is no way to make Vec sound for linear `T` without one of:

1. A *dispose protocol* — `Vec(T:Linear)` becomes itself Linear; the only way to drop it is via an explicit `Vec::dispose(self)` that requires (or runtime-asserts) `len == 0`. Users must `pop` every element individually before disposing. `clear` and indexed-write are unavailable.
2. *Per-element-aware methods* — `clear` and indexed-write take a consumer callback that explicitly receives each linear element; the user is responsible for consuming them.

Both designs are coherent but nontrivial, and both deserve careful thinking about realloc panic-paths (a partial realloc that aborts mid-copy must not leave linear elements unconsumed). v1 punts cleanly: sema rejects the type with an error pointing to this ADR, and a future ADR can introduce one of the two mechanisms above.

**Note on arrays:** the existing fixed-array machinery (`[T; N]`) does *not* currently propagate linearity through `is_type_linear` — `[MustUse; N]` is treated as non-linear and silently allows implicit element drops. This is a pre-existing soundness gap in arrays, not a deliberate design. This ADR does not try to fix the array gap; the same future ADR that adds linear-element support to Vec should also tighten array linearity by recursing through compound types in `is_type_linear`. Filed separately so it can be fixed independently of Vec landing.

### Drop

The compiler emits a per-`T` drop function for `Vec(T)`:

```text
fn __drop_Vec_T(v: Vec(T)):
    for i in 0..v.len:
        drop_in_place(&mut v.ptr[i])    // calls T's drop, if T has one
    if v.cap > 0:
        __gruel_free(v.ptr, v.cap * sizeof(T), align_of(T))
```

Generated inline at codegen time. For `T: Copy`, the drop loop collapses to nothing — the function is just the free.

### Implementation shape

- **`gruel-builtins`**: add `BuiltinTypeConstructorKind::Vec` and the `VEC_CONSTRUCTOR` entry. Update `BUILTIN_TYPE_CONSTRUCTORS`. (No `BuiltinTypeDef` — Vec is not a synthetic struct; it's a `TypeKind` variant with codegen-handled methods.)
- **`gruel-air`**: add `TypeKind::Vec(TypeId)` with intern-pool support. Sema lowers `Vec(T)` in type position. Type-checking for the method surface. Place-grammar: range subscripts on a Vec receiver yield slice borrows (uniform with arrays from ADR-0064 phase 4). Affine-ness, drop-glue generation (per-`T` drop function emission flag).
- **`gruel-intrinsics`**: add `VEC_METHODS` registry with each method's signature and codegen-lowering tag. Add `IntrinsicId::PartsToVec` to `INTRINSICS`. Update generated docs.
- **`gruel-codegen-llvm`**: per-method lowering routines emitting inline LLVM. Allocator calls go to existing `__gruel_alloc` / `__gruel_realloc` / `__gruel_free`. Per-`T` drop function emission. `terminated_ptr` lowers to "if `cap == len` { grow }; store `s` at `ptr[len]`; return `ptr`".
- **`gruel-runtime`**: no new runtime FFI functions. The existing allocator primitives suffice. (One small addition: a `__gruel_vec_grow` helper that encapsulates the doubling-capacity policy + realloc, called from the codegen'd `push`/`reserve`/`terminated_ptr`. Optional — could also be inlined.)
- **Borrow-checker** (post-ADR-0062): treat range subscripts on Vec like range subscripts on arrays; whole-Vec borrow during slice scope; mutation methods (including `terminated_ptr`) conflict with live slice borrows.
- **Spec**: new section under chapter 7 (alongside arrays and slices) covering Vec construction, methods, on-demand termination, drop, and slice integration.

### Migration

Same pattern as ADR-0061 / 0062 / 0063 / 0064:

1. Build behind `--preview vec`.
2. Land a parallel test suite under `crates/gruel-spec/cases/vec/`.
3. Stabilize and remove the gate.

`String` stays as-is. A future ADR can revisit the relationship.

## Implementation Phases

- [x] **Phase 1: Type system foundation** — add `TypeKind::Vec(TypeId)` with intern-pool support. LLVM lowering as `{ T*, i64, i64 }` aggregate. No surface form yet. Add `BuiltinTypeConstructorKind::Vec` and `VEC_CONSTRUCTOR` to `gruel-builtins`. Add `PreviewFeature::Vec` to `gruel-error`.

- [x] **Phase 2: Construction** — sema accepts `Vec(T)` in type position behind `--preview vec`. Reject `Vec(T)` when `T: Linear` with a clear error pointing at this ADR. Implement `Vec::new()` and `Vec::with_capacity(n)`. Codegen emits the constructor inline (zero-init the aggregate; for `with_capacity` with `n > 0`, call `__gruel_alloc`). Drop function emission for `T: Copy` (just free).

- [x] **Phase 3: Length / capacity / is_empty queries** — add `VEC_METHODS` registry skeleton; implement the three field-extraction methods. Codegen emits direct field GEPs.

- [x] **Phase 4: Push / pop / clear / reserve** — codegen emits inline grow-or-write for `push`; bounds-checked move-out for `pop`; len-reset (with element drop) for `clear`; grow-to-additional for `reserve`. The grow path uses `__gruel_realloc` with a doubling-capacity policy, optionally encapsulated in `__gruel_vec_grow`.

- [x] **Phase 5: `@vec` literal intrinsic** — add `IntrinsicId::Vec` to `gruel-intrinsics` with `--preview vec` gating. Sema unifies argument types to a single `T`, rejects empty calls and `T: Linear`. Codegen allocates `cap = n` slots, stores each argument by move, sets `len = n`. Reuses the alloc/grow path from Phase 4.

- [x] **Phase 6: Drop for non-Copy `T`** — codegen emits a per-`T` drop function that loops over `[0..len]` calling the element drop, then frees. Tested on `Vec(String)`, `Vec(Vec(i32))`. `clear` reuses the same drop loop.

- [x] **Phase 7: Indexing** — `v[i]` read for `T: Copy`, `v[i] = val` write. Bounds checks per spec 7.1:9–11. Move-out-of-non-Copy rejected per 7.1:28.

- [x] **Phase 8: Slice borrowing** — extend ADR-0064 phase 4's range-subscript place form to accept Vec receivers. `&v[..]` and `&mut v[a..b]` produce `Slice(T)` / `MutSlice(T)` of length `len`. Borrow-checker treats Vec mutation as conflicting with live slice borrows.

- [x] **Phase 9: Iteration** — for-each over `Vec(T)` lowers to for-each over `&v[..]`. Inherits Copy / non-Copy / mut-iter rules from ADR-0064 phase 8. Mut form deferred if deref-assignment hasn't landed.

- [x] **Phase 10: Checked-block extras** — `v.ptr()`, `v.ptr_mut()`, `v.terminated_ptr(s)`, `@parts_to_vec(p, len, cap)`. Each gated to `checked` blocks. `terminated_ptr` codegen: ensure `cap > len` (grow if needed via the same path as `push`'s grow); store `s` at `ptr[len]`; return the pointer.

- [x] **Phase 11: Clone** — `v.clone() -> Vec(T)` for `T: Clone` (per ADR-0065). Codegen allocates `cap` slots; for `T: Copy`, single `memcpy(len * sizeof(T))`; for non-Copy `T: Clone`, per-element clone loop calling `T::clone` via interface dispatch. Register `Vec(T): Clone where T: Clone` conformance. **v1 status (final): `T: Copy` only.** Sema explicitly rejects `Vec(T:non-Copy).clone()` with a clear error message rather than silently emitting the unsound shallow-memcpy. The earlier scoped-down implementation aliased the heap buffer for `Vec(String)` etc. — fixed by adding the rejection. Per-element clone for non-Copy elements requires field-of-borrow access (so the loop body can dispatch to `T::clone(&dst[i])`) and is deferred to a follow-up ADR alongside the analogous struct-clone-synthesis work in ADR-0065.

- [x] **Phase 12: `@vec_repeat` intrinsic** — add `IntrinsicId::VecRepeat` to `gruel-intrinsics` with `--preview vec` gating. Sema requires `T: Clone` (per Phase 11), `n: usize`, rejects `T: Linear`. Codegen allocates `cap = n` slots; for `n >= 1`, clones `v` into `ptr[0..n-1]` and moves `v` into `ptr[n-1]`; for `n == 0`, returns empty Vec and drops `v`. `T: Copy` collapses the clone path to a store loop / `memcpy`. **v1 status (final): `T: Copy` only**, same rationale as Phase 11. Sema rejects non-Copy `T` with a pointer to the deferred work.

- [x] **Phase 13: Spec** — author `docs/spec/src/07-arrays/03-vectors.md` covering type, layout, ownership, construction (including `@vec` and `@vec_repeat`), methods, on-demand termination, slice integration, iteration, drop, and the FFI / `checked` surface.

- [x] **Phase 14: Stabilize** — remove the `vec` preview gate, drop `PreviewFeature::Vec`, update ADR status to `implemented`. Migration ADR for `String → Vec(u8)` (if pursued) is a separate document.

### v1 limitations (future-work follow-ups)

- **`@vec_repeat(v, n)` requires `T: Copy`.** Non-Copy `T: Clone` needs per-element clone synthesis dispatching to the Clone interface. Depends on ADR-0065 Phase 2 (currently deferred there).
- **`v.clone()` for non-Copy `T: Clone`** uses the same memcpy path, which is incorrect for affine elements that own resources. Per-element clone synthesis is the same future work as @vec_repeat above.
- **`for x in v` mutable form** (yielding `MutRef(T)`) is deferred alongside the equivalent slice-mut iteration pending ADR-0062 phase 8's deref-assignment operator.

## Consequences

### Positive

- **First-class generic vector.** Any program needing a heap-backed list of `T` finally has a built-in answer. Drop, indexing, iteration, slice integration all work uniformly with the rest of the type system.
- **Same surface family as Slice / Ref / Ptr.** Construction, methods, and integration parallel ADR-0064. Users learning slices already know most of Vec.
- **Monomorphization, not type-erasure.** Per-`T` codegen means `push`/`pop`/`index` have no runtime indirection on element ops — just the inline LLVM you'd hand-write.
- **No new runtime FFI surface for the hot path.** The codegen-handled methods need only the existing allocator primitives. New FFI is limited to the `@parts_to_vec` intrinsic, which is `checked`-block-only.
- **Zero-cost when sentinel isn't used.** Layout is `{ ptr, len, cap }`. Mutation methods are pure Rust-Vec semantics with no extra branches. Programs that never call `terminated_ptr` pay nothing for the FFI feature.
- **Explicit FFI handoff.** Every `terminated_ptr(s)` call site names the sentinel value. No "set it up once and hope it's still maintained" — the call site that needs the contract is the one that establishes it.
- **Slice integration is free.** ADR-0064's place-grammar extension already handles range subscripts on places; adding "Vec is a place that supports range subscripts" is a small extension.
- **`String` is unaffected.** No migration risk; the existing String tests, runtime functions, and methods all keep working. Future `String → Vec(u8)` is a clean follow-up if desired.

### Negative

- **Sentinel does not persist across mutations.** Code that interleaves Vec mutation and FFI calls must call `terminated_ptr(s)` before each handoff. This is the right semantics, but a user porting from C (where the buffer "is always NUL-terminated because I always write the NUL") may need to learn the discipline.
- **Pointer-invalidation hazard.** `let p = v.terminated_ptr(0); v.push(x); call_c(p);` is unsound if the push reallocated. Same as Rust's `Vec::as_ptr` + `push` hazard. `checked`-block discipline applies.
- **`terminated_ptr` requires `&mut self`.** A function holding `&Vec(T)` cannot call it; it must hold `&mut Vec(T)` or own the Vec. Slightly less ergonomic than a hypothetical `&self` form, but unavoidable since the method may grow the buffer.
- **Drop emission becomes a per-`T` codegen task.** The compiler now emits a `__drop_Vec_T` function for every monomorphized `Vec` element type. Increases binary size proportional to the number of distinct element types that need non-trivial drops — typical monomorphization tax.
- **No type-level FFI guarantee.** A function signature can't say "this Vec is always NUL-terminated" — that's a property of a momentary `terminated_ptr` call, not the type. Users doing complex FFI dataflow may want the type-tracked sentinel that's listed under future work.

### Neutral

- **No new IR concepts.** Vec is a `TypeKind` variant; methods lower to existing IR + allocator calls. The borrow-checker reuses the array / slice machinery.
- **Test surface grows.** A new spec test directory under `crates/gruel-spec/cases/vec/`, plus golden tests for the codegen output of representative methods.
- **Doc surface grows.** Builtins reference page picks up Vec as a new type constructor; spec gains a new chapter.

## Open Questions

1. **What's the doubling policy for grow?** Standard 2x is the obvious answer. `String` uses 2x (`__gruel_string_realloc`). Vec should match. Worth confirming the minimum-capacity-on-first-grow constant — `String` uses 16 bytes; for typed Vec, `max(4 elements, 32 / sizeof(T))` or similar. Nail down in phase 4.

2. **Should `terminated_ptr` accept `usize` extra capacity for FFI calls that need a larger buffer than `len + 1`?** Some C APIs want a sized output buffer that you'll later know the length of (e.g. `getcwd`-style). For now, keep `terminated_ptr(s) -> Ptr(T)` minimal. A `terminated_ptr_with_extra(s, n)` could be added if the pattern shows up.

3. **Should `terminated_ptr` and `ptr` / `ptr_mut` all sit behind `checked`?** ADR-0064 puts every raw-pointer extraction behind `checked` for consistency. We follow suit. Argument for relaxing `terminated_ptr` specifically: the contract it establishes (`ptr[len] == s` immediately after the call) is checkable by inspection of the codegen — no UB hides in the method itself. Argument against: the *consumer* of the returned pointer is what makes it dangerous, and that consumer is FFI by definition. Stay conservative; require `checked`.

4. **Should there be a `Vec::from_array(arr: [T; N]) -> Vec(T)` constructor?** Useful for building Vecs from literals. Out of scope for v1 (depends on whether array values can be moved into heap allocations — a codegen question that touches arrays-as-rvalues). Note as future work.

## Future Work

- **`String` migration.** A separate ADR can redefine `String` as a thin wrapper over `Vec(u8)` with a UTF-8 invariant, removing the `String__*` runtime functions and unifying the byte-string story. The current ADR is deliberately compatible.
- **Allocator parameterization.** `Vec(T, A)` once an `Allocator` interface stabilizes.
- **Rich method surface.** `extend`, `insert`, `remove`, `swap_remove`, `truncate`, `drain`, `dedup`, `sort`, `from_iter`, `to_vec` (slice → Vec), `into_iter` (consuming iteration). Each is a small registry add.
- **More `Option(T)`-returning accessors.** `get(i) -> Option(T)`, `first() -> Option(T)`, `last() -> Option(T)`, `find(pred) -> Option(T)`. Build on the same `Option(T)` from ADR-0065 that `pop` already uses.
- **Mut iteration.** Once deref-assignment lands (ADR-0062 / 0064 phase 8 dep).
- **Split-borrows over Vec ranges.** `&mut v[0..3]` and `&mut v[3..6]` simultaneously when borrow-checker can prove disjointness. Same future work as ADR-0064.
- **Capacity tuning.** `shrink_to_fit`, `try_reserve` (returning failure rather than panicking on OOM), capacity hints for builders.
- **`Vec::from_array` and array-rvalue support.** Build a Vec from an array literal.
- **Empty `@vec()`.** Inferring `T` from a surrounding type ascription (`let v: Vec(i32) = @vec();`) requires bidirectional sema — deferred to keep v1's `@vec` arity-≥1 and unidirectional. Use `Vec::new()` for empty construction in v1.
- **Linear element support** (paired with array linearity fix). Add a dispose protocol or per-element-aware methods so `Vec(T:Linear)` becomes usable. The same change should fix the pre-existing gap where `[T:Linear; N]` silently allows implicit element drops by making `is_type_linear` recurse through compound types.

## References

- ADR-0020: Built-in Types as Synthetic Structs (the `String` mechanism — *not* what Vec uses)
- ADR-0025: Comptime and Monomorphization
- ADR-0028: Unchecked Code and Raw Pointers
- ADR-0041: For-each Loops
- ADR-0050: Intrinsics Crate
- ADR-0061: Generic Pointer Types (`Ptr` / `MutPtr`)
- ADR-0062: Reference Types Replacing Borrow Modes
- ADR-0063: Pointer Operations as Methods on Ptr / MutPtr
- ADR-0064: Slices (`Slice` / `MutSlice`) — direct sibling
- ADR-0065: `Clone` Interface and Canonical `Option(T)` — hard prereq for `Vec::clone` and `Vec::pop`
- Spec ch. 7: Fixed-Size Arrays and Slices
- [Rust: `Vec<T>`](https://doc.rust-lang.org/std/vec/struct.Vec.html) and [`CString::from_vec_with_nul`](https://doc.rust-lang.org/std/ffi/struct.CString.html#method.from_vec_with_nul)
- [Zig: `std.ArrayList`](https://ziglang.org/documentation/master/std/#std.array_list.ArrayList) and [Sentinel-Terminated Arrays](https://ziglang.org/documentation/master/#Sentinel-Terminated-Arrays)

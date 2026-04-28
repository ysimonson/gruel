---
id: 0064
title: Slices
status: proposal
tags: [types, syntax, slices, arrays, pointers]
feature-flag: slices
created: 2026-04-27
accepted:
implemented:
spec-sections: ["7.2"]
superseded-by:
---

# ADR-0064: Slices

## Status

Proposal

## Summary

Introduce `Slice(T)` and `MutSlice(T)` as scope-bound, non-owning views over a contiguous run of `T` values. A slice is a fat pointer (`{ ptr, len }`) that supports bounds-checked indexing, length queries, and (in `checked` blocks) raw-pointer extraction. Construction is syntactic, mirroring `&x` / `&mut x` for refs: `&arr[..]` produces a `Slice(T)` view of an array, `&mut arr[lo..hi]` produces a sub-range `MutSlice(T)`. Slices have an *optional sentinel contract* at construction — `&arr[..n :s]` (Zig-style) produces a slice whose follow-on element is guaranteed to be `s`. The contract is honored by construction, not tracked by the type, so sentinel-dependent operations (`slice.terminated_ptr()`) live behind `checked`. Naming follows the ADR-0061 / ADR-0062 convention (`MutPtr` / `MutRef`).

## Context

Gruel today has fixed-size arrays (`[T; N]`, codified in spec chapter 7) and pointers (`Ptr(T)` / `MutPtr(T)` after ADR-0061). There is no way to:

1. **Pass a contiguous run of values without committing to a length at the type level.** `fn sum(xs: [i32; 4]) -> i32` works for arrays of length 4 only. To handle any length, today's options are (a) a new function per length, (b) a heap-allocated `Vec`-like type that doesn't exist yet, or (c) raw `Ptr(i32)` plus a separate length argument and `checked` blocks everywhere.
2. **Take a sub-range view of an array.** `arr[1..3]` is unimplemented (ADR-0030 deferred subslice projection); the range grammar isn't reserved either.
3. **Interoperate with C-shaped APIs that expect `(buf, len)` pairs or null-terminated buffers.** Manual `(Ptr(T), usize)` tuples work but lose the bounds-checking discipline Gruel has elsewhere.

Slices are the standard answer. Zig and Rust both have them; their representations differ in detail but share the fat-pointer shape.

### What Zig does

- `[]T` — slice of `T` (ptr + len).
- `[*]T` — many-item pointer (no length).
- `[:0]T` — sentinel-terminated slice; the type system tracks the sentinel value `0`.
- `[N:0]T` — fixed-size array with terminator.
- Slices are first-class values, return-able, store-able. Lifetime is the programmer's responsibility (allocator-managed).

### What Rust does

- `&[T]` / `&mut [T]` — borrowed slice. Lifetime is tracked by the borrow checker.
- `[T]` is unsized; slices appear only behind a reference.
- No sentinel slices — null-terminated C strings are handled via `CStr` (a separate type) and FFI shims.

### Where Gruel sits

Gruel has scope-bound `Ref(T)` / `MutRef(T)` (ADR-0062) — borrowing without lifetimes. Slices are the multi-element generalization. The non-escape rules from ADR-0062 carry over verbatim. Lifetimes (and thus stored / returned slices) are deferred to the same future-work bucket as stored refs.

Sentinel slices are useful enough to want today (FFI to C strings) but type-tracking them adds complexity (registry needs to encode comptime values). This ADR takes the lighter path: sentinels are a construction-time invariant, FFI extraction is a `checked` operation. Promoting sentinels into the type system is reserved as future work if the contract approach proves error-prone.

## Decision

### Types

- `Slice(T)` — read-only view of `n` contiguous `T` values.
- `MutSlice(T)` — read-write view (parallels `MutPtr` / `MutRef`).

Internal: `TypeKind::Slice(TypeId)` and `TypeKind::MutSlice(TypeId)`, interned like `TypeKind::PtrConst` / `PtrMut`. LLVM lowering: a struct `{ ptr: T*, len: i64 }` passed in two registers (System V ABI) or as an aggregate on stacks that need it.

### Semantics — scope-bound, mirroring `Ref` / `MutRef`

- A `Slice(T)` cannot be mutated through.
- A `MutSlice(T)` is exclusive — at most one live `MutSlice` to overlapping storage at a time, and no concurrent `Slice`s.
- Slices cannot be stored in struct fields, returned from functions, or captured by closures that outlive the function. (Same non-escape rule as ADR-0062.)
- Slices borrow from a place. Producing a slice from `arr` follows the same exclusivity book-keeping as `&arr` / `&mut arr`.

### Construction

Slices are constructed by borrowing a *range subscript* of an array, mirroring `&x` / `&mut x` for refs. The construction operator is `&` (or `&mut`); the place under it is the array indexed by a range.

```gruel
let arr: [i32; 5] = [1, 2, 3, 4, 5];

// Whole-array views
let s: Slice(i32) = &arr[..];               // immutable view of all 5
let m: MutSlice(i32) = &mut arr[..];        // mut view; requires `let mut arr`

// Sub-range views
let mid:  Slice(i32)    = &arr[1..4];       // [2, 3, 4]
let mmid: MutSlice(i32) = &mut arr[1..4];

// Sentinel form — guarantees arr[hi] == sentinel and that arr[hi] is in-bounds
let line: Slice(u8)    = &bytes[..n :0];    // bytes[n] == 0
let cmd:  MutSlice(u8) = &mut bytes[..n :0];
```

This is uniform with `&x` / `&mut x` for refs (ADR-0062): `&` is the borrow-construction operator, and the kind of borrow you get is determined by the place you borrow. A plain place yields `Ref(T)` / `MutRef(T)`; a range subscript yields `Slice(T)` / `MutSlice(T)`. No special-cased "constructor methods" — `&arr[..]` parses, type-checks, and borrow-checks via the same path as `&arr`.

The result's scope is bound to the receiver's place by the borrow checker, exactly as for `Ref` / `MutRef`.

#### Range expressions

This ADR introduces ranges *only in subscript position*:

```ebnf
range  = expression ".." expression                  (* a..b   *)
       | expression ".."                             (* a..    *)
       | ".." expression                             (* ..b    *)
       | ".."                                        (* ..     *)
       ;

range_with_sentinel
       = range ":" expression                        (* a..b :s *)
       ;

subscript = "[" ( expression | range | range_with_sentinel ) "]" ;
```

Ranges are not yet a general-purpose expression form (no `Range` type, no for-each over `0..n`); they are syntax recognized by the subscript parser. Promoting ranges to first-class expressions is future work that doesn't depend on this ADR.

The endpoints follow Rust/Zig: `a..b` is half-open `[a, b)`. Bounds checks: `a <= b <= N` (compile-time when constant, runtime otherwise). Sentinel form additionally checks `arr[b]` is in-bounds and equals `s`.

#### Raw construction in `checked` blocks

For FFI and unchecked work, slices can be assembled from raw pointers via two `@`-prefixed intrinsics added to the `INTRINSICS` registry:

```gruel
checked {
    let p: Ptr(u8) = ...;
    let s: Slice(u8) = @parts_to_slice(p, len);
    let q: MutPtr(u8) = ...;
    let m: MutSlice(u8) = @parts_to_mut_slice(q, len);
}
```

The element type is inferred from the pointer argument: `@parts_to_slice(p: Ptr(T), n: usize) -> Slice(T)` and `@parts_to_mut_slice(p: MutPtr(T), n: usize) -> MutSlice(T)`. Like all intrinsics, these live in `INTRINSICS` (gruel-intrinsics) and lower in `translate_intrinsic` (gruel-codegen-llvm). The non-escape rule still bans user-defined function signatures from naming `Slice` / `MutSlice` in return position; intrinsics are the closed exception list.

### Methods

| Form | Receiver | On | Signature |
|------|----------|----|-----------|
| `s.len()` | method | `Slice(T)`, `MutSlice(T)` | `(self) -> usize` |
| `s.is_empty()` | method | `Slice(T)`, `MutSlice(T)` | `(self) -> bool` |
| `s[i]` | indexed read | `Slice(T)`, `MutSlice(T)` | `(self, i: usize) -> T` (Copy types) |
| `s[i] = v` | indexed write | `MutSlice(T)` only | `(self, i: usize, v: T) -> ()` |
| `s.ptr()` | method, `checked` | `Slice(T)`, `MutSlice(T)` | `(self) -> Ptr(T)` |
| `s.ptr_mut()` | method, `checked` | `MutSlice(T)` only | `(self) -> MutPtr(T)` |
| `s.terminated_ptr()` | method, `checked` | `Slice(T)`, `MutSlice(T)` | `(self) -> Ptr(T)` |
| `@parts_to_slice(p, n)` | `@`-intrinsic, `checked` | — | `(p: Ptr(T), n: usize) -> Slice(T)` |
| `@parts_to_mut_slice(p, n)` | `@`-intrinsic, `checked` | — | `(p: MutPtr(T), n: usize) -> MutSlice(T)` |

Indexing follows the same bounds-checking rules as fixed arrays (spec 7.1:9–11): constant indices checked at compile time, variable indices checked at runtime, out-of-bounds panics.

Non-Copy element handling follows spec 7.1:28: reading via `s[i]` for a non-Copy type is rejected (would move out of indexed position). Future `swap` / `take` methods can lift this; out of scope.

### Sentinel discipline

The sentinel form `&arr[lo..hi :s]` performs three checks at construction:

1. The byte at `arr[hi]` is in-bounds of the source array.
2. `arr[hi] == s`.
3. The view itself is non-empty in the array-borrow form — `lo < hi` is required, so `arr[hi]` exists as a real follow-on byte. (Empty sentinel slices over a one-byte buffer are constructible only via `@parts_to_slice` / `@parts_to_mut_slice` in a `checked` block.)

If any check fails, the program panics. After construction, the slice's runtime representation is identical to a non-sentinel slice — `{ptr, len}`, no extra fields, no per-slice flag. The sentinel guarantee survives only as a fact the programmer remembers. Operations that depend on it (`terminated_ptr()`) are `checked`-only because the type system isn't tracking the invariant.

This is a deliberately weaker guarantee than Zig's `[:0]T`. The trade is one register per slice and zero new comptime-value-in-type machinery, in exchange for `terminated_ptr()` being a programmer responsibility. If the contract approach proves error-prone in real use, a future ADR can promote sentinels into the type system without breaking this ADR's surface form.

### Iteration

Slices integrate with ADR-0041 for-each loops:

```gruel
for x in s {                    // x: T (Copy)
    total = total + x;
}

for x in m {                    // x: MutRef(T) — write through (mut form of for-each)
    *x = *x + 1;
}
```

For-each lowering treats a slice as an iterator over `0..s.len()`, projecting via `s[i]`. The mut form requires the `*x = ...` deref-assignment that's also blocking ADR-0062's phase-8 cleanup; this ADR depends on that deref operator landing first (or no-op-`mut`-iterates as an interim).

### Place-expression integration

`arr[range]` is a *place expression* — it names a sub-place of `arr`, just as `arr[i]` names a single-element place. The `&` / `&mut` operators (ADR-0062) already produce a borrow of any place; this ADR extends the place grammar with range subscripts and the type rules so that `&arr[range]` produces `Slice(T)` instead of `Ref(T)`.

Concretely, the `&place` rule from ADR-0062 reads the place's category to pick the borrow type:

| place under `&` | borrow type |
|-----------------|-------------|
| `x`, `s.f`, `arr[i]` (single index) | `Ref(T)` |
| `arr[range]` (range subscript) | `Slice(T)` |

`&mut` mirrors the table with `MutRef` / `MutSlice`. No new operator, no special method registry — slice construction is just borrow construction over the new place form.

Range subscripts are recognized in the parser only inside `[ … ]`. They are not yet a general expression form (no first-class `Range` value). The lexer needs no new tokens — `..` is already valid (or trivially produced by the existing `.` token rule).

### What this ADR does NOT include

- **Range as a first-class expression.** `let r = 1..3;` and `for i in 0..n` do not work. Ranges live only in subscript position. A future range-expressions ADR can lift this and would automatically extend slicing too.
- **Slice-of-slice subscripting.** `s[1..3]` (range subscript on a slice receiver) is left for the same future ADR — once slices can take range subscripts, the same `&` rules apply.
- **Lifetimes / stored slices.** Slices are scope-bound; future work, same bucket as stored refs.
- **Type-tracked sentinels.** A future `Slice(T, sentinel)` form is left as future work.
- **Slice-of-slice (nested slices over multi-dim arrays).** Out of scope. `s[i]` returns `T`, not `Slice(_)`.

### Implementation shape

- `gruel-lexer` / `gruel-parser`: add range subscript parsing (`..`, `a..b`, `a..`, `..b`, plus `:s` sentinel suffix) inside `[ … ]`. No new tokens; `..` is a sequence the parser recognizes in the subscript context.
- `gruel-builtins`: add `SLICE_CONSTRUCTOR` and `MUT_SLICE_CONSTRUCTOR` to `BUILTIN_TYPE_CONSTRUCTORS`, with `BuiltinTypeConstructorKind::Slice` and `BuiltinTypeConstructorKind::MutSlice` (parallel to the existing `Ptr`/`MutPtr`/`Ref`/`MutRef` entries).
- `gruel-air`: add `TypeKind::Slice(TypeId)` and `TypeKind::MutSlice(TypeId)`, interned like the pointer pool. Add a place-category for range subscripts so the `&` / `&mut` rules can dispatch on it.
- `gruel-intrinsics`: add `SliceKind` (`Slice`/`MutSlice`), `SliceMethod`, and `SLICE_METHODS` (mirror of `POINTER_METHODS`). Add `IntrinsicId::PartsToSlice` and `IntrinsicId::PartsToMutSlice` to the existing `INTRINSICS` registry (the same registry that hosts `@cast`, etc.), each gated to `checked` blocks. No `ARRAY_METHODS` registry — array → slice construction is the borrow operator over a range subscript, not a method call.
- `gruel-codegen-llvm`: lower `TypeKind::Slice(_)` / `MutSlice(_)` to a `{ptr, i64}` aggregate; implement intrinsics for each method (bounds check + GEP for indexing, cast for `ptr()`, panic-or-return for `from_raw_parts` / sentinel checks). Lower `&arr[range]` to a fat-pointer construction emitting the bounds check and the offset-pointer GEP.
- `gruel-runtime`: panic helpers for range-construction failures (range bounds, sentinel mismatch).
- Borrow checker (post-ADR-0062): treat `Slice(T)` / `MutSlice(T)` like `Ref(T)` / `MutRef(T)` for exclusivity and non-escape. Range subscripts borrow the whole place (same conservatism as `&arr` borrowing the whole array); split-borrow over disjoint ranges is future work.

### Migration

Same pattern as ADR-0061 / 0062 / 0063:

1. Build behind `--preview slices`.
2. Land a parallel test suite under `crates/gruel-spec/cases/slices/`.
3. Stabilize and remove the gate. (No legacy syntax to retire — slices are wholly new.)

## Implementation Phases

- [x] **Phase 1: Type system** — add `TypeKind::Slice(TypeId)` and `TypeKind::MutSlice(TypeId)` with intern-pool support. LLVM lowering as `{ptr, i64}`. No surface form yet.
- [x] **Phase 2: Constructor registry** — add `SLICE_CONSTRUCTOR` and `MUT_SLICE_CONSTRUCTOR` to `BUILTIN_TYPE_CONSTRUCTORS`. Sema accepts `Slice(T)` and `MutSlice(T)` in type position. Gate behind `--preview slices`.
- [x] **Phase 3: `len` and `is_empty`** — add `SLICE_METHODS` registry; implement `s.len()` and `s.is_empty()` for both slice variants. Codegen extracts the length field from the fat pointer.
- [x] **Phase 4: Range subscripts in place position** — parser recognizes `..`, `a..b`, `a..`, `..b` inside `[ … ]`. AIR / sema add a "range subscript" place category. `&arr[range]` produces `Slice(T)`; `&mut arr[range]` produces `MutSlice(T)`. Bounds-check `lo <= hi <= N` (compile-time when constant; runtime otherwise). Borrow-check the receiver: `&mut` requires a mutable place; both forms produce a scope-bound borrow.
- [x] **Phase 5: Indexing** — `s[i]` for read on `Slice(T)` / `MutSlice(T)`, `s[i] = v` for write on `MutSlice(T)`. Bounds checks per spec 7.1:9–11. Move-out-of-non-Copy rejected per 7.1:28.
- [x] **Phase 6: Checked-block extras** — `s.ptr()`, `s.ptr_mut()`, `@parts_to_slice(p, n)`, `@parts_to_mut_slice(p, n)`. Each gated to `checked` exactly like ADR-0028's pointer intrinsics.
- [x] **Phase 7: Sentinel subscripts** — extend the range subscript parser with `:s` suffix; `&arr[lo..hi :s]` / `&mut arr[lo..hi :s]` lower to a sentinel-checking borrow construction. `s.terminated_ptr()` (checked) lands here. Construction-time sentinel verification panics on mismatch; the runtime helper for the panic message lives in `gruel-runtime`.
- [x] **Phase 8: Iteration** — for-each over `Slice(T)` (yields `T` for Copy types) and `MutSlice(T)` (yields `MutRef(T)`). The mut form is gated on the deref-assignment operator that ADR-0062 phase 8 calls out as a prerequisite — if that hasn't landed, this phase ships the immutable iteration only and the mut form follows up. *Status: immutable form shipped; mutable form deferred until ADR-0062 phase 8 lands the deref-assignment operator.*
- [ ] **Phase 9: Spec** — author a new `docs/spec/src/07-arrays/02-slices.md` covering types, construction, methods, sentinel discipline, scope-bound rules, and iteration. Update `01-fixed-arrays.md` with the new range-subscript place form.
- [ ] **Phase 10: Stabilize** — remove the `slices` preview gate, drop `PreviewFeature::Slices`, update ADR status to `implemented`.

## Consequences

### Positive

- **Generic over length.** Functions and views finally compose without committing to a fixed `N`.
- **Same surface family as pointers and refs.** `Slice(T)` / `MutSlice(T)` parallel `Ptr(T)` / `MutPtr(T)` and `Ref(T)` / `MutRef(T)`. Construction follows the borrow-operator convention (`&x` / `&mut x` for refs, `&arr[range]` / `&mut arr[range]` for slices). Methods on the slice value follow ADR-0063's `POINTER_METHODS` pattern.
- **No special-case constructor surface.** `&arr[..]` is borrow-of-place over a new place form, not a compiler-emitted method. The non-escape rule applies uniformly to user-written signatures; the `checked`-block escape hatch (`from_raw_parts`) is the same intrinsic-on-type-call surface ADR-0063 already opened.
- **Cheap.** `{ptr, len}` only — no per-slice sentinel flag, no extra word. FFI hand-off via `terminated_ptr()` costs zero space.
- **Composes with for-each (ADR-0041).** Iteration is uniform with arrays.

### Negative

- **Sentinel discipline is the programmer's job.** A `Slice` that "has a sentinel" is indistinguishable at runtime from one that doesn't. Misuse — calling `terminated_ptr()` on a non-sentinel slice — is checked-block-only but still possible. Future ADR can promote sentinels into the type system if this proves error-prone.
- **New place-grammar form.** Range subscripts are only valid in `[ … ]` and only as a place under `&` / `&mut` (using a range subscript as an rvalue, e.g. `let r = arr[1..3];`, is rejected — there is no slice value without a borrow). This is one new grammar rule plus a place-category, but no new operator.
- **Mut iteration depends on deref.** `for x in m` on a `MutSlice(T)` needs `*x = v` semantics; that's an outstanding gap from ADR-0062 phase 8. Phase 8 here either ships partial or gets sequenced after the deref ADR.
- **Empty sentinel slices via raw parts only.** Edge case, but worth noting: `&arr[i..i :s]` is rejected so the contract stays "sentinel byte exists at len".
- **Whole-place borrow conservatism.** `&arr[0..2]` and `&arr[2..4]` aren't simultaneously borrowable in safe code even though they're disjoint — same conservatism as `&arr` borrowing the whole array. Split-borrow is future work.

### Neutral

- **No new IR concepts beyond fat pointers.** LLVM aggregate handling is well-trodden.
- **Borrow-checker reuse.** Slices flow through the same exclusivity / non-escape rules as refs.

## Open Questions

1. **Range endpoints.** `a..b` is half-open `[a, b)` (Rust/Zig). `from_raw_parts(p, n)` takes a length, not an endpoint pair, since there's no second pointer to bound against. Same convention as each community already uses.
2. **Should `s.ptr()` and `s.ptr_mut()` require `checked` even though the slice is bounds-checked?** Today's pointer surface treats *any* extraction of a `Ptr(T)` as `checked`. Slices inheriting that is consistent. Argument the other way: a slice's pointer is "safer" than `Ptr(T)::from(&x)` because the slice already proves the storage is valid. For now, conservative: require `checked`. Cheap to relax later.
3. **Does `@parts_to_slice` / `@parts_to_mut_slice` accept `n == 0`?** Yes, with `p` arbitrary (including null). Mirrors `&[]` / Zig's empty slice. Sentinel form is the only one that requires non-empty.
4. **`s[i]` for non-Copy types.** Spec 7.1:28 rejects move-out-of-array; this ADR mirrors that for slices. Future `swap` / `take` methods come with the same future ADR that lifts the array restriction.

## Future Work

- **Ranges as first-class expressions.** A first-class `Range(T)` type would let `for i in 0..n { ... }` work and would let slice-of-slice subscripting (`s[1..3]`) fall out of the same machinery as array subscripting.
- **Split-borrows.** Allow `&mut arr[0..2]` and `&mut arr[2..4]` simultaneously when the borrow checker can prove disjointness. Independent extension; doesn't change this ADR's surface.
- **Type-tracked sentinels.** Promote sentinels into the type (`Slice(T, S)` shape), removing the `checked`-block requirement on `terminated_ptr()`. Requires extending `BuiltinTypeConstructor` to accept comptime values, not just types.
- **Stored / returned slices.** Lifetimes for refs (ADR-0062 future work) extend to slices on the same machinery.
- **Slice-of-slice / multi-dim views.** Once stored slices land, nested views become useful.
- **Capability-based allocators.** A `Vec(T)` parameterised over an `Allocator` interface returns owned storage that exposes a `MutSlice(T)` view. Designed in its own ADR; this ADR deliberately leaves the interface unprejudged.
- **`copy_from_slice` and friends.** `m.copy_from(other_slice)`, `m.fill(v)`, etc. Easy adds once the registry is in place.

## References

- Spec ch.7: Fixed-Size Arrays
- ADR-0028: Unchecked Code and Raw Pointers
- ADR-0030: Place Expressions (deferred subslice projection)
- ADR-0041: For-each Loops
- ADR-0061: Generic Pointer Types
- ADR-0062: Reference Types Replacing Borrow Modes
- ADR-0063: Pointer Operations as Methods on Ptr / MutPtr
- [Zig: Slices](https://ziglang.org/documentation/master/#Slices)
- [Rust: Slice Type](https://doc.rust-lang.org/reference/types/slice.html)

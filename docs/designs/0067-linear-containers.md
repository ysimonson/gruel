---
id: 0067
title: Linear Types in Containers (Vec, Array, Option)
status: implemented
tags: [types, linearity, ownership, collections]
feature-flag:
created: 2026-05-01
accepted: 2026-05-01
implemented: 2026-05-01
spec-sections: ["7.3"]
superseded-by:
---

# ADR-0067: Linear Types in Containers (Vec, Array, Option)

## Status

Implemented (with `Option(T:Linear)` prelude support deferred — see Phase 3).

## Summary

Make compound types linearity-aware: a container holding a linear element type is itself linear, propagating the must-consume obligation to whoever holds the container. Concretely:

1. **`is_type_linear` recurses** through `[T; N]`, `Vec(T)`, and enums whose variants carry data — closing the soundness gap noted in ADR-0066's "Linear elements" section where `[MustUse; N]` silently allowed implicit drops.

2. **`Vec(T)` accepts linear element types**, with an explicit `Vec::dispose(self)` method that panics if `len != 0`. The existing `is_clone`-style sema rejection of `Vec(T:Linear)` is removed; the compiler now propagates linearity through the type and lets the linear-discipline checker enforce explicit consumption.

3. **`Option(T)` for linear `T`** also gets a `dispose(self)` method that panics if `Some` (i.e., requires the variant to be `None`). `unwrap` for `Option(T:Linear)` is rejected because the panic path would leak the contained linear value; users must `match` exhaustively.

4. **Arrays of linear types** become first-class linear values via the recursion fix. Because partial moves are banned (ADR-0036), there's no clean drainage protocol; the user must pass the whole array to a consumer function or destructure it. No `dispose` method on arrays — the existing linear-value-not-consumed diagnostic governs them.

The shared `dispose` mechanism unifies the "I'm done with this container, please free its storage but verify I didn't accidentally leak" pattern across `Vec` and `Option`. Containers of non-linear types continue to drop normally (existing behavior preserved).

## Context

ADR-0008 introduced linear types: values that must be explicitly consumed and cannot be implicitly dropped. Today the linearity check only inspects the top level: a `linear struct MustUse` reports `is_type_linear == true`, but `[MustUse; 4]` and `Vec(MustUse)` and `Option(MustUse)` all evaluate as non-linear because the recursion stops at the compound layer.

ADR-0066 documented this as a known gap (the array case): it shows `let _arr: [MustUse; 1] = [...];` compiling cleanly even though dropping `_arr` implicitly drops a linear value. That ADR's "Linear elements" subsection explicitly named the fix as a follow-up and rejected `Vec(T:Linear)` outright to avoid carrying the bug forward.

ADR-0065's `Option(T)::unwrap` open question (OQ2) flagged the same issue: panicking through a partially-consumed Option would leak the linear payload.

The user-facing model the language wants is straightforward:

- A container holding a linear element type is itself linear.
- Disposing of an "empty" linear container (no live elements) is the documented escape hatch.
- Disposing of a non-empty one is a runtime panic — symmetric to the implicit-drop rejection at compile time.

The two open soundness gaps and the missing `dispose` shape are the same problem viewed from three angles. Solving them together keeps the rules coherent.

### What's already in place

- **ADR-0008**: linearity discipline (`linear struct`, "linear value must be consumed" check).
- **ADR-0036**: partial moves banned (simplifies the array story by ruling out per-element moves).
- **ADR-0066**: `Vec(T)` infrastructure, including methods registered via `dispatch_vec_method_call`. Adding a `dispose` method is one more entry there.
- **ADR-0065**: `Option(T)` infrastructure via the prelude. Methods (`is_some`, `unwrap`, etc.) live in the prelude source string; adding `dispose` is one more entry there.
- **`is_type_linear`** has two parallel definitions in `sema/builtins.rs` and `sema_context.rs` — both need the same recursion fix.

### What this ADR does *not* attempt

- **`dispose` for arrays.** Fixed-N arrays don't have a runtime `len`; checking "no elements live" would either always succeed (if `N == 0`) or always panic (if `N > 0`), neither of which is useful. The linearity recursion alone makes arrays of linear types behave correctly under the existing must-consume discipline — that's enough for v1.
- **Drainage protocols for arrays of linear types.** Because partial moves are banned, the user can't drain elements one at a time. They must pass the whole array to a function that destructures it, or use a future for-each-consume form (out of scope; depends on a separate iteration ADR).
- **Forwarding `Drop` interface conformance to linear containers.** `Vec(T:Linear)` doesn't conform to `Drop` (per ADR-0059's rule that linear types don't conform). Generic code that requires `T: Drop` won't accept a linear-element vector. Right call — users opting into linearity opt out of implicit-drop generic code.
- **Per-element clone for Vec(T:Linear).clone().** Linear types don't conform to `Clone` (would create a second linear value out of one); `Vec(T:Linear).clone()` is rejected via the existing `T: Clone` constraint.

## Decision

### Linearity recursion

Both `is_type_linear` implementations (`sema/builtins.rs` and `sema_context.rs`) extend their match to recurse through compound types:

```rust
pub fn is_type_linear(&self, ty: Type) -> bool {
    match ty.kind() {
        TypeKind::Struct(id) => self.type_pool.struct_def(id).is_linear,
        // New cases:
        TypeKind::Array(id) => {
            let (elem, _) = self.type_pool.array_def(id);
            self.is_type_linear(elem)
        }
        TypeKind::Vec(id) => {
            let elem = self.type_pool.vec_def(id);
            self.is_type_linear(elem)
        }
        TypeKind::Enum(id) => {
            let def = self.type_pool.enum_def(id);
            def.variants
                .iter()
                .any(|v| v.fields.iter().any(|f| self.is_type_linear(*f)))
        }
        _ => false,
    }
}
```

Tuples don't need a special arm because tuple types are encoded as structs (per ADR-0048 / lookup of tuple-shaped struct names); the existing struct arm + the user-defined struct's `is_linear` flag picks them up if any field is linear.

A consequence: enums with one or more linear-payload variants become linear. For `Option(T:Linear)`, `Some(T)` carries a linear value, so `Option(T:Linear)` is linear. The all-enums-are-Copy v1 simplification (§3.8:2) is **not** changed by this ADR — Copy and Linear are still mutually exclusive at the predicate level (`is_type_copy` doesn't return true for linear types under the recursion-aware check), but the discriminant-only `Copy` shortcut in `is_type_copy` for enums needs a guard to avoid claiming a linear enum is Copy. This is a small targeted update, *not* a generalization to "enums are Copy iff payloads are Copy" (that's a bigger refactor for a future ADR).

### Vec(T:Linear)

Sema removes the rejection introduced in ADR-0066 Phase 2. `Vec(T:Linear)` is now accepted at type-resolution time. The linearity propagation makes the Vec itself linear: `Vec(MustUse)` is `is_type_linear == true`.

A new method joins the dispatch table:

```text
fn dispose(self)  →  ()
```

Codegen emits inline:

```text
if self.len != 0:
    __gruel_vec_dispose_panic()  // diverges
free(self.ptr, self.cap * sizeof(T), align)
```

`__gruel_vec_dispose_panic` is a new entry in `gruel-runtime` that prints `"panic: Vec::dispose called on a non-empty Vec\n"` and exits 101.

The existing Vec drop continues to free the buffer, but now sema's linear-value-not-consumed check rejects implicit drops of `Vec(T:Linear)`, so the only paths that reach the buffer-free are explicit `dispose` (for emptied Vecs) or `pop` until empty followed by dispose. Users must drain the Vec themselves; the compiler doesn't auto-emit a drop loop on linear elements.

`dispose` is registered in `VEC_METHODS` for both `Vec(T:Copy/Affine)` (where it's an alternative to implicit drop — semantically equivalent to drop on a non-linear Vec, but explicit) and `Vec(T:Linear)` (where it's the *only* legal release path).

### Option(T:Linear) — deferred to follow-up

Naively adding `dispose` (and using existing `is_some`/`unwrap`) to the prelude `Option(T)` doesn't work for linear `T`. The prelude defines all methods generically, and any of:

```gruel
fn is_some(borrow self) -> bool {
    match self {
        Self::Some(_) => true,
        Self::None => false,
    }
}
```

instantiated with linear `T` triggers a sema error: the discard pattern `Self::Some(_)` on a *borrowed* enum tries to "move out" of the borrow, even though no payload is actually consumed. The pattern-matcher doesn't yet have a special-case for "borrow-position discard of a linear payload."

Closing this gap properly requires either:

1. **Smarter pattern-on-borrow handling.** Recognize that `Self::Some(_)` against `borrow self` doesn't actually consume anything and accept the match. Touches the discriminant-extraction path in sema and is a self-contained change but non-trivial.
2. **Per-T method gating in the prelude.** Allow methods to be conditionally instantiated based on `T`'s ownership — e.g. `is_some` available only when `T: Copy/Affine`, `dispose` available only when `T: Linear`. Requires extending the comptime-generic-enum machinery.

Both options are larger than the dispose-mechanism this ADR is centered on, so `Option(T:Linear)` is deferred to a follow-up ADR. This ADR's Phase 1 still makes `Option(T:Linear)` *report* as linear (preventing implicit drop), and the user must currently `match` exhaustively at the use site rather than relying on prelude methods.

### Arrays of linear types

No new method. The linearity recursion fix is sufficient: `[T:Linear; N]` is `is_type_linear == true`, the existing linear-value-not-consumed check fires on implicit drops, and the user must explicitly consume the array (pass it to a function, destructure it via `let [a, b, c] = arr`, etc.). For-each consumption is deferred — the existing for-each-array path doesn't handle move-out semantics for linear elements cleanly, and forcing that to work requires a partial-move story that ADR-0036 deliberately ruled out.

The bug-fix nature of this change is documented in the spec; the soundness gap noted in ADR-0066 is closed.

### Drop synthesis interaction

`drop_names::type_needs_drop` already recurses through compound types; no change needed there. The codegen `__gruel_drop_*` functions for affine compound types (e.g. a struct with a `Vec(T:Linear)` field) work correctly because:

1. The struct itself is now linear (linearity propagation), so the struct can't be implicitly dropped either.
2. If the user explicitly disposes of the struct, the dispose protocol cascades.

For `Vec(T:Affine)` — non-linear, non-Copy elements like `Vec(String)` — the existing drop loop runs, dropping each element. Unchanged.

For `Vec(T:Linear)` — the codegen-emitted Vec drop function would *also* need to handle the per-element drop, but since linear types can't be implicitly dropped, this code path is dead under the linear discipline. The codegen still emits the drop loop for safety but sema rejects the path that would reach it.

### Future-work boundary

- **`Drop` interface for linear containers.** Currently `Vec(T:Linear)` doesn't conform to `Drop`; generic code requiring `T: Drop` won't accept it. Could be loosened later if practice shows demand.
- **Array dispose / drainage.** A future ADR can add a `[T; N]::take(self) -> [T; N]` form or a partial-move-friendly for-each that handles linear elements; the linearity fix here doesn't preclude either.
- **Allocator parameterization.** ADR-0066's deferred allocator work doesn't interact with this ADR.
- **Linearity-aware unwinding.** ADR-0065's OQ2 still applies to `Option(T:Linear)::unwrap` — by rejecting it outright we sidestep the unwinding question for now.

## Implementation Phases

- [x] **Phase 1: Linearity recursion** — extend `is_type_linear` in both `sema/builtins.rs` and `sema_context.rs` to recurse through `Array`, `Vec`, and `Enum` (linear iff any variant payload is linear). Update the `is_type_copy` enum arm to return false for linear enums (single guard line). Add unit tests for nested linear shapes.

- [x] **Phase 2: Vec(T:Linear) acceptance + dispose** — remove the `is_type_linear(arg_types[0])` rejection in `sema/typeck.rs`. Add `dispose` to `VEC_METHODS` dispatch and `IntrinsicId::VecDispose` to the intrinsics registry. Codegen emits the inline `len != 0 → panic ; free buffer` sequence. Add `__gruel_vec_dispose_panic` runtime function. Spec tests cover both `Vec(T:Linear)` and `Vec(T:Affine)` calling dispose.

- [x] **Phase 3: Option(T:Linear) — deferred.** Naive prelude additions don't compile because the existing methods (`is_some`, `unwrap`, etc.) pattern-match on `borrow self`, and the discard pattern `Self::Some(_)` against a borrowed enum with a linear payload triggers a "move out of borrow" error even though `_` consumes nothing. Phase 1's recursion still makes `Option(T:Linear)` *report* as linear (preventing implicit drop); users currently must `match` at the use site. A follow-up ADR will add either smarter pattern-on-borrow handling for discard patterns or per-T method gating in the prelude.

- [x] **Phase 4: Array linearity propagation** — covered by Phase 1 (`is_type_linear` recursion).

- [x] **Phase 5: Spec section** — section 7.3 of the language spec gains paragraphs 7.3:8 (dispose) and 7.3:9 (linear elements); the existing 7.3:1 / 7.3:2 entries update to remove the "T must not be linear" wording and to point to the propagation rule.

- [x] **Phase 6: Stabilize** — no preview gate was ever added; the recursion fix and dispose method are unconditional. Update ADR status.

## Consequences

### Positive

- **Closes a known soundness gap.** ADR-0066 explicitly noted the array case as a follow-up; this ADR delivers it.
- **Unifies the dispose pattern.** `Vec::dispose` and `Option::dispose` share semantics (panic if non-empty, free otherwise), making the linear-container experience predictable.
- **Removes the Vec(T:Linear) blanket ban.** Users with linear types (file handles, capabilities, etc.) can now use Vec to manage collections of them, as long as they discipline themselves to drain before disposal.
- **Type-system honesty.** Compound types with linear payloads now report their linearity correctly; tools and human readers see the same answer.

### Negative

- **No drainage protocol for arrays.** Users with `[T:Linear; N>0]` who want to drop the array can't currently do so without passing it to a consumer function. Awkward but not broken; future ADR can add a `take` / for-each-consume form.
- **Vec(T:Linear).clone is rejected.** Cloning a linear value isn't well-defined (would create a second linear obligation), so `Clone` doesn't conform — but generic code that bounds `T: Clone` won't accept `Vec(T:Linear)`. Same trade as for individual linear values.
- **`unwrap` rejection for linear T may surprise users.** `let x = opt.unwrap()` is the idiomatic way to extract a value; rejecting it for linear T forces the user into `match`. The error message must be clear.
- **Runtime cost on dispose.** A branch on `len != 0` per dispose call. Negligible but not zero.
- ~~**Two `is_type_linear` definitions to keep in sync.**~~ Resolved in this ADR's implementation: the recursion logic now lives once on `TypeInternPool::is_type_linear`; both `Sema::is_type_linear` and `SemaContext::is_type_linear` delegate to it.

### Neutral

- **No new IR concepts.** Vec dispose is one more intrinsic in the existing registry; Option dispose is one more method in the prelude source.
- **No allocator changes.** Existing `__gruel_alloc` / `__gruel_free` suffice.

## Open Questions

1. **Should `Vec(T:Affine).dispose()` be allowed?** Semantically it's redundant — affine Vecs drop normally — but allowing it as an explicit alternative to implicit drop has documentary value ("I'm done with this Vec on purpose"). Tentative: allow. Costs nothing.

2. **Should `Vec(T:Linear).dispose()` panic-message include element type info?** Static info is cheap. Tentative: yes, include `T`'s name in the panic message via a per-monomorphization string, like Rust's panic messages do.

3. **Should there be a `Vec::drain_dispose(self, f: fn(T))` convenience?** Drain elements via a callback, then dispose. Useful but adds method-with-comptime-fn-arg complexity. Punted to follow-up.

4. **Do we want to surface a separate diagnostic for "dispose called on Vec(T:Linear) with len != 0" vs the generic panic message?** Yes — error code, structured diagnostic, the works. Cheap to add.

## Future Work

- **Drainage protocols for arrays.** A `[T; N]::take(self) -> [T; N]` or for-each-consume that handles partial-moves cleanly.
- **Linearity-aware unwinding.** Lift the `Option(T:Linear)::unwrap` rejection by making panic paths drop the partial state correctly.
- **`Drop` interface for linear containers** under explicit opt-in, if generic code patterns demand it.
- **Layout optimization for `Option(Ptr(T))`** (carried over from ADR-0065 future work, unaffected by this ADR).

## References

- ADR-0008: Affine and linear ownership.
- ADR-0036: Partial moves banned.
- ADR-0059: Drop and Copy as Interfaces.
- ADR-0065: Clone Interface and Canonical Option(T) — flags the dispose / linear question for Option.
- ADR-0066: Vec(T) — documents the array linearity gap and rejects Vec(T:Linear) pending this ADR.

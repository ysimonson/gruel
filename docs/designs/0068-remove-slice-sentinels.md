---
id: 0068
title: Remove Slice Sentinel Support
status: implemented
tags: [slices, ffi, simplification, removal]
feature-flag:
created: 2026-05-01
accepted: 2026-05-01
implemented: 2026-05-01
spec-sections: ["7.2"]
superseded-by:
---

# ADR-0068: Remove Slice Sentinel Support

## Status

Implemented

## Summary

Remove the slice sentinel surface introduced by ADR-0064 phase 7: the `&arr[lo..hi :s]` / `&mut arr[lo..hi :s]` construction form, the construction-time sentinel-equality check, and the `terminated_ptr()` methods on `Slice(T)` and `MutSlice(T)`. The feature was a weak, programmer-tracked invariant — the type system never recorded the sentinel, `terminated_ptr()` is currently just an alias for `ptr()` (same `IntrinsicId::SlicePtr`), and the "guarantee" only survived as a fact the user remembered. ADR-0066's `Vec(T).terminated_ptr(s)` covers the dominant FFI use case (build a buffer, hand a NUL-terminated pointer to C) with an *on-demand* model that's both more honest about the cost (one capacity check + one byte write per handoff) and explicit at every call site. This ADR strips the slice sentinel surface from the parser, RIR/AIR, codegen, intrinsics registry, spec, and tests.

## Context

ADR-0064 phase 7 added an optional `:s` suffix to range subscripts:

```gruel
let arr: [u8; 6] = [b'h', b'i', 0, b'?', b'?', b'?'];
let s: Slice(u8) = &arr[0..2 :0];   // verifies arr[2] == 0
checked {
    let p: Ptr(u8) = s.terminated_ptr();  // hand to C
}
```

The construction-time check verifies `arr[hi] == s` and `lo < hi <= N - 1` (so the sentinel byte is in-bounds and non-degenerate). The check panics on mismatch. After construction, the slice's runtime representation is identical to a non-sentinel slice — there is no per-slice flag, no extra word, no type-system tracking. `terminated_ptr()` is `checked`-only and currently lowers to the same `IntrinsicId::SlicePtr` as `ptr()`; the only thing the sentinel form buys is the one-time construction equality check.

ADR-0064 itself flagged this as a deliberately weaker design than Zig's type-tracked `[:0]T` ("Sentinel discipline is the programmer's job. A `Slice` that 'has a sentinel' is indistinguishable at runtime from one that doesn't"). The plan was to revisit if the contract approach proved error-prone in real use. Since then:

1. **`Vec(u8)` landed (ADR-0066)** with `terminated_ptr(s) -> Ptr(T)` that ensures `cap > len`, writes `s` at `ptr[len]`, and returns the pointer. This is *on-demand* termination — the sentinel is established at the FFI boundary, not maintained as an invariant. The dominant "build a string, pass to C" workflow is now first-class without any slice sentinel involvement.

2. **Slice `terminated_ptr()` is functionally indistinguishable from `ptr()`.** Both entries in `SLICE_METHODS` (Slice and MutSlice variants of `terminated_ptr`) point at `IntrinsicId::SlicePtr` with the same `slice_ptr` lowering. The "pay attention to the sentinel" promise lives entirely in the variable name and a comment in the spec.

### What removing this costs

Exactly one capability disappears: zero-copy hand-off of an *already-terminated* fixed array to C. Concretely, `&arr[0..2 :0]` on `[u8; 3] = [b'h', b'i', 0]` lets you produce a `Ptr(u8)` pointing at `b'h'` with the guarantee that `arr[2] == 0`, without copying. After this ADR, the equivalent paths are:

- **Copy into a Vec**: `let v: Vec(u8) = @vec(b'h', b'i'); checked { v.terminated_ptr(0) }`. One heap allocation, one `memcpy` of the live bytes, one byte write. For typical short strings this is single-digit nanoseconds.
- **Raw construction in `checked`**: `checked { @parts_to_slice(... )` or compute the pointer manually. Still available, no bounds-checked sugar.

The use cases where the cost matters — large already-terminated buffers handed to C without copying — are uncommon and well-served by the raw `checked` escape. The use cases the surface form *suggested* it covered (string-literal FFI, building NUL-terminated buffers) are all better served by `Vec(u8)` or by a future `c"..."` literal.

## Decision

Delete the slice sentinel surface in one ADR-scoped change:

### Removed surface

1. **Parser**: the `:s` suffix on range subscripts. After this change, the only forms accepted inside `[ … ]` (in place position) are `..`, `a..b`, `a..`, `..b`. The `RangeExpr::sentinel: Option<Box<Expr>>` field is removed; `chumsky_parser.rs`'s `sentinel_suffix` combinator and its two consumer sites in the range-subscript productions are removed; the AST printer drops the `: <expr>` rendering.

2. **RIR / AIR**: the `sentinel: Option<InstRef>` / `Option<AirRef>` field on the range-subscript instruction (`RangeBorrow` / equivalent) is removed. Astgen no longer emits a sentinel operand. RIR / AIR printers drop the `, sentinel=…` rendering.

3. **Sema**: in `analyze_ops.rs`'s range-borrow handling, the `sentinel_opt` branch is deleted along with the `strict` bounds tightening (`lo < hi` and `hi < N`) that only applied to the sentinel form. Non-sentinel bounds (`lo <= hi <= N`) are unchanged. The sentinel-typecheck (sentinel must be a constant of element type) is removed.

4. **CFG / codegen**: in `codegen.rs`'s range-subscript lowering, the sentinel runtime equality check (load `arr[hi]`, compare to `s`, panic on mismatch) is removed. The `CfgRangeSubscript` `sentinel: Option<CfgValue>` field is removed. The runtime panic helper specifically for sentinel mismatch (if a dedicated one exists) is removed from `gruel-runtime`; if it's the generic range-bounds panic, it stays.

5. **Intrinsics registry** (`gruel-intrinsics/src/lib.rs`): the two `SliceMethod` entries named `"terminated_ptr"` (one for `SliceKind::Slice`, one for `SliceKind::MutSlice`) are removed from `SLICE_METHODS`. Their `intrinsic` was already `IntrinsicId::SlicePtr` (i.e. they were aliases of `ptr` / `ptr_mut`), so no `IntrinsicId` variant disappears. The `Vec(T).terminated_ptr(s)` method is **untouched** — it is its own intrinsic (`IntrinsicId::VecTerminatedPtr` / `vec_terminated_ptr`) registered in `VEC_METHODS`, with substantively different semantics (write-then-return).

6. **Spec** (`docs/spec/src/07-arrays/02-slices.md`): the "Sentinel form" subsection of the construction grammar is removed; the `terminated_ptr()` entry in the slice methods table is removed; the "Sentinel discipline" prose section is removed. The grammar rule for `range_with_sentinel` is removed from the appendix grammar. Paragraph numbering in `02-slices.md` is renumbered as needed; downstream `spec = [...]` references in tests are migrated.

7. **Spec tests**: `crates/gruel-spec/cases/slices/sentinel.toml` is deleted in full (all five cases). No replacements.

### Unchanged

- `Vec(T).terminated_ptr(s)` (ADR-0066). Same signature, same checked-block requirement, same on-demand semantics.
- `Slice::ptr()` and `MutSlice::ptr()` / `MutSlice::ptr_mut()`. Still `checked`-only, still return raw element pointers.
- Non-sentinel range subscripts (`&arr[..]`, `&arr[a..b]`, etc.) and their bounds-check semantics. Unchanged.
- `@parts_to_slice` / `@parts_to_mut_slice` (ADR-0064 phase 6). Unchanged.

### Migration

There is no user-facing migration: no preview gate, the feature ships in stable as of ADR-0064 phase 10, and there are no known external consumers. Internal consumers are limited to:

- `crates/gruel-spec/cases/slices/sentinel.toml` — deleted.
- Any in-repo `.gruel` examples that use the `:s` form — none found at audit time, but `grep -rn ' :[0-9]' --include='*.gruel'` should be re-run before landing.

If a downstream user does have a `:s` form, the migration is one of:

```gruel
// Before
let s: Slice(u8) = &arr[0..n :0];
checked { let p = s.terminated_ptr(); call_c(p); }

// After (Vec — heap copy)
let v: Vec(u8) = Vec::with_capacity(n);
for i in 0..n { v.push(arr[i]); }
checked { let p = v.terminated_ptr(0); call_c(p); }

// After (raw — no copy, no bounds check on the sentinel)
checked {
    // user is asserting arr[n] == 0 themselves
    let p: Ptr(u8) = @parts_to_slice(arr.as_ptr(), n).ptr();
    call_c(p);
}
```

Both alternatives are explicit about the trade. The error message at the parse site for `:s` should be friendly and point at this ADR for rationale (see Phase 1).

### Why a single phase per crate, not a preview-gate ramp

This is pure deletion. Preview-gating the *removal* of a stabilized feature would be ceremony — there is no half-state where the parser accepts `:s` but sema rejects it that's any better than just rejecting at the parser. We delete top-down (parser → RIR → AIR → CFG → codegen → intrinsics → spec → tests) in one ADR with phases scoped by crate so each phase is independently committable.

## Implementation Phases

- [x] **Phase 1: Parser** — remove `sentinel_suffix` from `chumsky_parser.rs`'s range-subscript productions; remove `RangeExpr::sentinel` field from `gruel-parser/src/ast.rs`; remove the AST printer's sentinel rendering. The parse error for an attempted `:s` form should be the natural "expected `]`, found `:`" — clear enough without bespoke text. Update parser unit tests that specifically constructed sentinel `RangeExpr`s.

- [x] **Phase 2: RIR** — remove the `sentinel: Option<InstRef>` field from the range-subscript variant in `gruel-rir/src/inst.rs`; remove `renumber_opt(*sentinel)` from the renumber impl; remove the printer's `, sentinel=…` rendering; remove sentinel emission from `gruel-rir/src/astgen.rs`'s range-subscript handler.

- [x] **Phase 3: AIR** — remove the `sentinel: Option<AirRef>` field from the range-subscript variant in `gruel-air/src/inst.rs`; remove the printer rendering; remove sentinel handling from `gruel-air/src/sema/analyze_ops.rs` (the `sentinel_opt` branch, the `strict` bounds tightening, and the result write-back). Remove any other `sentinel: None` constructors that exist purely to satisfy the field. Remove sentinel branch from `gruel-air/src/inference/generate.rs`.

- [x] **Phase 4: CFG / codegen** — remove `sentinel: Option<CfgValue>` from `gruel-cfg/src/inst.rs`'s range-subscript instruction; remove the construction in `gruel-cfg/src/build.rs`; remove the printer rendering. In `gruel-codegen-llvm/src/codegen.rs`, remove the sentinel runtime check block (load `arr[hi]`, compare, panic-on-mismatch). Remove any `gruel-runtime` panic helper that exists exclusively for sentinel mismatch (verify it's not shared with bounds-check panics first).

- [x] **Phase 5: Intrinsics registry** — remove the two `SliceMethod` entries named `"terminated_ptr"` from `SLICE_METHODS` in `gruel-intrinsics/src/lib.rs`. Verify no `IntrinsicId` variant becomes orphaned (`SlicePtr` is still used by `ptr` / `ptr_mut`). Run `make gen-intrinsic-docs` and commit the regenerated `docs/generated/intrinsics-reference.md`.

- [x] **Phase 6: Spec** — edit `docs/spec/src/07-arrays/02-slices.md` to remove the "Sentinel form" construction subsection, the `terminated_ptr()` row from the methods table, and the "Sentinel discipline" prose section. Renumber affected paragraphs. Update grammar appendix to drop `range_with_sentinel`. Migrate any traceability `spec = [...]` references in surviving tests if paragraph numbers shifted.

- [x] **Phase 7: Tests + cleanup** — delete `crates/gruel-spec/cases/slices/sentinel.toml`. Run `make test` to confirm no other tests reference removed paragraph IDs or use `:s` syntax. Add a regression test in `crates/gruel-ui-tests/cases/diagnostics/` (or wherever fits) verifying the parse-error wording for `&arr[0..2 :0]` is reasonable. Final search: `grep -rn ':[0-9]\|terminated_ptr' crates/ docs/` to confirm nothing leaks.

## Consequences

### Positive

- **Smaller surface.** One grammar form, one optional IR field across three IRs, one runtime check in codegen, two intrinsic-registry entries, and ~70 lines of spec all go away. Future grammar / IR / codegen work on range subscripts has one less variant to consider.
- **Honest cost model.** The remaining FFI termination story (`Vec(u8).terminated_ptr(s)`) is *exactly one* mechanism, with explicit per-call cost. Users no longer have to learn the distinction between "slice sentinel (construction-time, programmer-tracked)" vs. "vec terminated_ptr (on-demand, write-and-return)".
- **No misleading guarantee.** The slice `terminated_ptr()` method name suggested a property that the type system didn't actually enforce; removing it eliminates that footgun without a deprecation period.
- **Removes a dead alias.** Slice `terminated_ptr()` was already `IntrinsicId::SlicePtr` — semantically identical to `ptr()`. The two-name surface implied a difference that didn't exist.

### Negative

- **Zero-copy already-terminated array hand-off is gone.** The narrow case of `[u8; N]` containing a known NUL at a known position can no longer be handed to C without either (a) copying into a `Vec(u8)` or (b) dropping into `checked` and assembling the pointer by hand. Neither is a hardship; both are arguably more honest about what's happening.
- **One backwards-incompatible parse change.** Anyone with `&arr[a..b :s]` in their codebase gets a parse error. Mitigated by the feature being effectively unused (one in-tree test, no documented external users).
- **Spec paragraph renumbering.** Test traceability references that target paragraphs after the removed sections in `02-slices.md` need updating. Mechanical, but a small chore.
- **Slight asymmetry in the FFI story.** `Vec` has on-demand termination, fixed arrays have nothing.

### Neutral

- **No `IntrinsicId` variants disappear.** `SlicePtr` is retained for `Slice::ptr` / `MutSlice::ptr` / `MutSlice::ptr_mut`. The closed-enum exhaustive matches don't change.
- **Borrow-checker is unaffected.** The sentinel form had no borrow-checker implications beyond the underlying range subscript, which stays.
- **No runtime cost change for any non-sentinel program.** The removed runtime check only fired when the user wrote `:s`; programs that didn't were already paying nothing.

## Open Questions

1. **Should the parse error for `&arr[0..2 :0]` carry a hint?** No, remove it entirely to maximize LOC savings.

2. **Does removing the `range_with_sentinel` grammar rule simplify the appendix grammar in any cascading way?** Likely just a rule deletion plus the adjacent prose. Confirm during phase 6.

## References

- ADR-0064: Slices — introduces the slice sentinel surface this ADR removes (phase 7).
- ADR-0066: `Vec(T)` — the on-demand `terminated_ptr(s)` model that subsumes the FFI use case.
- ADR-0028: Unchecked Code and Raw Pointers — the `checked`-block escape that remains the answer for zero-copy raw FFI.
- ADR-0050: Intrinsics Crate — the registry surface that loses two `SLICE_METHODS` entries.
- Spec ch. 7.2: Slices — the section that loses the sentinel subsections.
- Zig: [Sentinel-Terminated Arrays](https://ziglang.org/documentation/master/#Sentinel-Terminated-Arrays) — the type-tracked design Gruel deliberately did *not* adopt; left as future work if real demand emerges.

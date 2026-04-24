---
id: 0054
title: Use usize for Indexing
status: proposal
tags: [types, indexing, ergonomics]
feature-flag: usize_indexing
created: 2026-04-23
accepted:
implemented:
spec-sections: ["3.1", "7.1", "3.7"]
superseded-by:
---

# ADR-0054: Use usize for Indexing

## Status

Proposal

## Summary

Tighten Gruel's indexing and size/length APIs to use `usize` exclusively: array index operands must be of type `usize` (not any unsigned integer), integer literals in index/length contexts infer to `usize`, and built-in "size-like" APIs (`String::len`, `String::capacity`, `@size_of`, `@align_of`) return `usize`. This resolves the open question deferred in ADR-0046 and aligns Gruel with Rust's convention.

## Context

ADR-0046 introduced `isize`/`usize` but explicitly deferred the semantic question of *where* they must be used. Today:

- Array indexing (`arr[i]`) accepts any unsigned integer type (`u8`, `u16`, `u32`, `u64`, `usize`). The check is `index_result.ty.is_unsigned()` in both `analyze_inst_for_projection` and `analyze_index_set_impl`.
- Integer literals in index position infer to `u64` (`test_array_index_literal_infers_u64`).
- `String::len()` / `String::capacity()` return `u64`.
- `@size_of(T)` / `@align_of(T)` return `i32`.
- A handful of size-ish intrinsic arguments (`@memcpy` count, etc.) are checked against `Type::U64`.

This is a portability and ergonomics wart:

1. **Portability**: On a hypothetical 32-bit target, `usize` would be 32-bit. Code that writes `let i: u64 = ...; arr[i]` works today but wouldn't on 32-bit. A 32-bit compile should reject such code at compile time, not silently narrow.
2. **Ergonomics**: Heterogeneous size/length types (`u64` from `.len()`, mixed with `usize` for pointer arithmetic) force awkward `@cast` calls.
3. **Consistency**: Rust uses `usize` for this universally and it's what systems programmers expect.

The change is feasible now because on all current targets (64-bit), `usize` and `u64` have identical representation. No runtime ABI changes; this is a purely compile-time type-checking tightening.

## Decision

### Indexing (arrays)

Array index operands **MUST** have type `usize`.

- Non-literal, non-`usize` operands are rejected with a clear error and a suggestion to bind `x` as `usize` or wrap with `@cast` in a `usize`-typed context (e.g., `let i: usize = @cast(x); arr[i]`).
- Integer literals in index position infer to `usize` (previously `u64`).
- `comptime_int` in index position coerces to `usize` (already handled by the existing coercion machinery — just changes the target type).

### Size/length APIs

Built-in APIs that semantically return "a count, length, size, or index" return `usize`:

| API | Before | After |
|---|---|---|
| `String::len()` | `u64` | `usize` |
| `String::capacity()` | `u64` | `usize` |
| `String::with_capacity(cap)` (param) | `u64` | `usize` |
| `@size_of(T)` | `i32` | `usize` |
| `@align_of(T)` | `i32` | `usize` |

Other intrinsics whose argument is conceptually a count/size (e.g., `@memcpy` count, `@memset` count) are updated to require `usize` rather than `u64`.

### Runtime ABI

The `String__len`, `String__capacity`, `String__with_capacity` runtime functions already pass/return `uint64_t`. Since `usize` on current targets is 64-bit, the C-side signatures don't change; only the Gruel-side types do.

### Infrastructure

Introduce `BuiltinParamType::Usize` and `BuiltinReturnType::Usize` variants in `gruel-builtins` so methods can declare `usize` parameters and return types without going through `U64`.

### Preview gate

New behavior gated behind `--preview usize_indexing`. While the flag is off, the old permissive rules apply (any unsigned integer is a valid index; literals default to `u64`; built-in APIs return `u64`). Once all tests pass under the flag, the flag is removed and the old behavior is deleted.

### Migration

Existing Gruel code that uses `u64` for indexing or length variables must be updated to `usize`. The compiler should suggest the minimal fix in its diagnostic (change the annotation to `usize`, or assign through a `usize`-typed `let` and use `@cast`). Spec tests and scratch programs will be updated in lockstep.

## Implementation Phases

- [x] **Phase 1: Preview flag + infrastructure**
  - Add `PreviewFeature::UsizeIndexing` to `gruel-error` (`name()`, `adr()`, `all()`, `FromStr`).
  - Add `BuiltinParamType::Usize` and `BuiltinReturnType::Usize` in `gruel-builtins`; wire through the mapping sites in `gruel-air` (analysis.rs ~lines 7916, 7940, 8041, 8069).
  - No behavior change yet; flag exists but is inert.

- [x] **Phase 2: Enforce `usize` on array indexing**
  - In `analyze_inst_for_projection` and `analyze_index_set_impl` (`gruel-air/src/sema/analysis.rs`), replace the `is_unsigned()` check with a `== Type::USIZE` check, gated behind `require_preview(UsizeIndexing, ...)`.
  - Make integer literals in index position infer to `usize` instead of `u64` when the flag is on.
  - Update the two existing tests (`test_array_index_type_must_be_unsigned` → still passes, `test_array_index_literal_infers_u64` → rename + assert `usize`).
  - Update spec section 7.1:7 from "MUST be an integer type" (currently loose) to "MUST be of type `usize`". Add a note on literal inference and a `@cast` migration example.

- [ ] **Phase 3: Size/length builtins return `usize`**
  - Update `String::len`, `String::capacity`, `String::with_capacity` in `gruel-builtins/src/lib.rs` to use `Usize`.
  - Runtime extern signatures in `gruel-runtime` stay as `uint64_t` (same layout).
  - Update spec sections covering `String` methods.

- [ ] **Phase 4: `@size_of` / `@align_of` return `usize`**
  - Change return type in `gruel-intrinsics` registry.
  - Update sema (`analyze_type_intrinsic`) and the comptime evaluator (`ConstValue::Integer` path in analysis.rs ~7784) — values are already integers, only the stamped type changes.
  - Update spec section covering these intrinsics and any spec tests that assert `i32` return.
  - Regenerate `docs/generated/intrinsics-reference.md` (`make gen-intrinsic-docs`).

- [ ] **Phase 5: Size-parameter intrinsics require `usize`**
  - In analysis.rs, change the `Type::U64` checks for count/size parameters (lines ~8936, 9170, 9290 — `@memcpy`, `@memset`, raw-pointer ops) to require `Type::USIZE`.

- [ ] **Phase 6: Migrate existing Gruel code under the flag**
  - Update spec tests in `crates/gruel-spec/cases/` that currently use `u64` indices or `u64` lengths (`arrays/fixed.toml:294`, `runtime/bounds.toml:17,33,146`, plus any tests that call `.len()` / `.capacity()` and bind the result). Tests should pass with `--preview usize_indexing`.
  - Update scratch examples / benchmarks in `crates/gruel-benchmarks` and anywhere else that indexes with `u64`.
  - Add new spec tests covering: `usize` index happy path, `u32` index rejected (with error-message check), literal-in-index inferred as `usize`, `String::len()` returning `usize`.

- [ ] **Phase 7: Stabilize**
  - Once phases 2–6 are green, remove the `require_preview(UsizeIndexing, ...)` gate.
  - Delete the old `is_unsigned()`-accepting path and the `u64` literal-in-index default.
  - Remove `PreviewFeature::UsizeIndexing`.
  - Update ADR status to `implemented`; update ADR-0046's open-question section to point at this ADR as the resolution.
  - Run `make test` + traceability check.

## Consequences

### Positive

- Portable array indexing: code compiles to identical semantics on 32-bit and 64-bit targets, or fails compile on the 32-bit target if the programmer hardcoded `u64`.
- One canonical "size" type throughout the language — no impedance mismatch between `u64` lengths and `usize` pointer offsets.
- Matches Rust; fewer surprises for systems programmers.
- Enables future work (slices, `Vec<T>`, `HashMap<K, V>`) to use `usize` uniformly from day one.

### Negative

- Breaking change for all existing Gruel code that uses `u64` for indexing or stores lengths. Mitigated by preview gating + clear error messages, but every spec test and scratch program needs updating.
- `@size_of` returning `usize` instead of `i32` means some arithmetic patterns (e.g., `@size_of(T) * -1`) that quietly worked with signed `i32` now fail type-checking. This is the right behavior but will surface latent bugs in existing uses.
- Adds `Usize` variants to `BuiltinParamType` / `BuiltinReturnType`, forcing every exhaustive match to grow an arm (one-time cost).

## Open Questions

- **Should `isize` ever be valid as an array index?** No — negative indices are meaningless for Gruel arrays. Keep `usize` only.
- **Should we allow `u32` / `u64` with implicit widening to `usize`?** No. ADR-0046 committed to "no implicit conversion between `isize`/`usize` and fixed-width integers." Keep that commitment; require `@cast`.
- **Error message suggestion form.** When a user writes `arr[i]` with `i: u64`, should the diagnostic suggest going through `@cast` (which requires a `usize`-typed context) or suggest changing `i`'s annotation to `usize`? Probably both, with the annotation change as the primary suggestion since it's cheaper at runtime and usually what the user meant.

## Future Work

- Slice types (`&[T]`) will naturally inherit `usize` indexing.
- `Vec<T>::push` / `Vec<T>::len` use `usize` from introduction.
- Range expressions (`0..n`) when introduced should default to `usize` in index contexts.
- Deprecation of any remaining `u64`-typed "count" APIs in `gruel-runtime` once the C ABI gains the flexibility.

## References

- ADR-0046 (Extended Numeric Types) — introduces `usize`; open question 3 explicitly defers this decision to a separate ADR.
- ADR-0050 (Intrinsics Crate) — registry that `@size_of` / `@align_of` are updated in.
- Rust Reference: `usize` and `std::ops::Index` — the convention we're adopting.

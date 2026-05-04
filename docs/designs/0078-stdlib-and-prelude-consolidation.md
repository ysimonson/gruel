---
id: 0078
title: Stdlib and Prelude Consolidation — Move Built-in Type Surface to Gruel Source
status: proposal
tags: [stdlib, prelude, builtins, runtime, refactor]
feature-flag: stdlib_consolidation
created: 2026-05-03
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0078: Stdlib and Prelude Consolidation

## Status

Proposal

## Summary

Move as much built-in type machinery as possible out of Rust source and into Gruel source files, in two parallel directions:

1. **Lift the synthetic prelude off a string literal and onto disk.** The current `PRELUDE_SOURCE` constant (`crates/gruel-compiler/src/unit.rs:90-316`, ~225 lines of Gruel embedded as a Rust raw-string) becomes one or more real `.gruel` files under the existing `std/` tree, loaded automatically before user code. ADR-0026 Phase 5 already shipped the on-disk stdlib resolution path; this ADR routes the prelude through the same mechanism.
2. **Collapse `String` to a Gruel-source newtype over `Vec(u8)`.** ADR-0072 made `String` structurally a `{ bytes: Vec(u8) }` newtype but kept the entire method surface in the Rust registry (`gruel-builtins/src/lib.rs:336-614`, ~280 LOC) backed by 31 `no_mangle` runtime functions in `gruel-runtime/src/string.rs` (751 LOC). Most of that runtime is now redundant — `s.len()` is `self.bytes.len()`, `s.clear()` is `self.bytes.clear()`, etc. This ADR finishes the collapse ADR-0072 anticipated in its summary ("Today's ~490 LOC … shrinks to the genuinely UTF-8-specific surface").

Concrete target: **eliminate ~900–1300 Rust LOC** across `gruel-runtime/src/string.rs`, `gruel-builtins/src/lib.rs`, and `crates/gruel-compiler/src/unit.rs`, replacing it with ~150–250 LOC of Gruel stdlib source. The runtime keeps only what is genuinely platform-bound (allocation, libc strlen, UTF-8 validation byte loop) or operator-bound (byte equality/ordering primitives the language can't yet express on `Slice(u8)`).

This ADR does **not** introduce new language features. It is a relocation of existing Gruel-expressible logic from Rust into Gruel source, with one small infrastructure change (loading prelude files from disk instead of from a string literal).

## Context

### Where things sit today

**Stdlib mechanism (already exists).** ADR-0026 Phase 5 shipped:

- `std/_std.gruel` — directory module root re-exporting `pub const math = @import("math.gruel");`
- `std/math.gruel` — `abs`, `min`, `max`, `clamp` (30 LOC of Gruel)
- `@import("std")` resolution lives in `crates/gruel-air/src/sema/analysis.rs:5620` (`resolve_std_import`), checking `GRUEL_STD_PATH` then `std/` relative to source
- Stdlib is **not** implicitly imported (per ADR-0026 §"Standard Library") — users write `const std = @import("std");`

**Prelude mechanism (already exists, but as a string).** Distinct from the stdlib:

- `crates/gruel-compiler/src/unit.rs:90` defines `const PRELUDE_SOURCE: &str = r#"…"#;` — a 225-line embedded Gruel program
- It contains: `Option(T)` and `Result(T, E)` (ADR-0065 / ADR-0070), `char__from_u32`, `char__is_ascii`, `char__len_utf8`, `char__encode_utf8` (ADR-0071), `Utf8DecodeError`, `String__from_utf8`, `String__from_c_str` (ADR-0072)
- Loaded under `FileId::PRELUDE` before user files (`unit.rs:526`); names are visible without `@import`
- Sema dispatches certain method/associated-function calls to prelude-resident free functions (e.g. `String::from_utf8(v)` → call `String__from_utf8(v)`; see `crates/gruel-air/src/sema/analysis.rs:4373-4395` and `:10913`)

**Built-in `String` (synthetic struct).** `gruel-builtins/src/lib.rs:336-614`:

- One field, `bytes: Vec(u8)` with `is_pub: false` (post-ADR-0072 layout)
- 6 operators (`==`, `!=`, `<`, `<=`, `>`, `>=`) each routing to `__gruel_str_eq` / `__gruel_str_cmp`
- 5 associated functions (`new`, `with_capacity`, `from_char`, `from_utf8_unchecked`, `from_c_str_unchecked`)
- 14 methods (`len`, `capacity`, `is_empty`, `clone`, `contains`, `starts_with`, `ends_with`, `concat`, `push_str`, `push`, `clear`, `reserve`, `bytes_len`, `bytes_capacity`, `into_bytes`, `push_byte`, `terminated_ptr`)
- Each entry is ~12-18 LOC of registry data; the slice spans ~280 LOC
- Drop glue: `__gruel_drop_String` declared in codegen at `crates/gruel-codegen-llvm/src/codegen.rs:936`

**Runtime backing.** `gruel-runtime/src/string.rs`:

- 31 `no_mangle` extern functions, 751 LOC
- Includes: byte equality/comparison (`__gruel_str_eq`, `__gruel_str_cmp`), allocator wrappers (`__gruel_alloc`, `__gruel_free`, `__gruel_realloc`, `__gruel_string_alloc`, `__gruel_string_realloc`), drop (`__gruel_drop_String`), one function per `String__*` method
- Many of these are pure delegations to byte-buffer ops that `Vec(u8)` already does inline in codegen

### What ADR-0072 said but didn't finish

ADR-0072 Summary (line 35): "Today's ~490 LOC in `gruel-runtime/src/string.rs` shrinks to the genuinely UTF-8-specific surface: validation (`__gruel_utf8_validate`), `from_c_str` ingest, and `terminated_ptr`'s NUL-write step."

The structural change shipped (String *is* `{ bytes: Vec(u8) }`), but the implementation kept the runtime functions intact and bolted them on top of the new layout. The collapse target is still open work.

### Why now

- The on-disk prelude/stdlib mechanism exists and is stable (ADR-0026 stable since 2026-01-04).
- `Vec(T)` is generic-monomorphized and inline-codegen'd (ADR-0066), so `String`'s methods can express themselves as compositions over `self.bytes` without a Rust round-trip.
- `Result(T, E)` (ADR-0070) and `char` (ADR-0071) are in the prelude and stable, removing the last user-visible blockers.
- The `PRELUDE_SOURCE` string has grown from a few dozen lines to ~225 and is starting to resist edits (no syntax highlighting, no per-file diffs, awkward escaping if anything ever needs a `"`). This is the right moment to move it.

### Constraints

- **Don't add new language features.** Generics, interfaces, drop-glue synthesis, and field-level moves all exist. This is pure relocation.
- **Don't break existing programs.** All current spec/UI tests must pass at every phase boundary.
- **Don't regress codegen quality.** A `String::len` call after this ADR should still lower to the same instructions (a load from the `len` slot of the underlying `Vec(u8)`). LLVM inlining handles small Gruel functions across the prelude boundary; the runtime FFI was never giving us optimization wins for these.
- **Field privacy stays.** The `bytes: Vec(u8)` field is non-pub. Prelude code can read it because the prelude lives under `FileId::PRELUDE` and ADR-0073's visibility check treats prelude/stdlib code as privileged for builtin field access (this is already how the inline prelude reaches `Vec(u8)` to construct a `String`).

## Decision

Three independent shifts, executed as separate phases so each lands a measurable Rust-LOC reduction.

### Shift 1: Prelude on disk

Replace the inline `PRELUDE_SOURCE` string with one or more on-disk files, loaded automatically when no explicit `@import` brings them in.

**Layout.** Add a `_prelude.gruel` (and supporting files if it grows) under the stdlib tree. Two viable locations, picking option (a):

(a) `std/_prelude.gruel` — colocated with `std/math.gruel`, resolved via the same `GRUEL_STD_PATH`/relative-`std/` logic in `resolve_std_import`. Reuses one resolution mechanism.

(b) Separate `prelude/` tree — clearer separation, but duplicates the resolution code.

**Loading.** `CompilationUnit::parse()` already prepends a synthetic prelude file (`unit.rs:523-541`). Replace the in-memory `SourceFile::new("<prelude>", PRELUDE_SOURCE, FileId::PRELUDE)` with a disk read keyed off the same `resolve_std_import` machinery, scoped to a sentinel filename like `_prelude.gruel`. If the file is absent (e.g. host without an installed stdlib), fall back to a hardcoded empty prelude — no error, because the prelude is purely additive.

**FileId discipline.** Keep `FileId::PRELUDE` as the file id assigned to whatever the loader returns, so all downstream code (visibility checks, span paths, ADR-0073's `is_accessible` privileged-access carve-out) keeps working unchanged.

**Test surface.** Tests that construct a `Sema` directly (e.g. `crates/gruel-air/src/sema/tests.rs:878`) need a way to inject the prelude without disk I/O. Provide a `PRELUDE_FALLBACK: &str` in `unit.rs` containing the same content as the disk file, used when disk lookup fails or in tests; the file on disk is the source of truth, the constant is a small fallback.

### Shift 2: Collapse String methods to Gruel

For each `String` method/assoc-fn whose body is expressible in Gruel today, remove the runtime function and the registry entry, and add a free function in the prelude (or a new `std/string.gruel` if we want the dispatching to feel less ad-hoc).

**Dispatch pattern (already in use).** `crates/gruel-air/src/sema/analysis.rs:4731` — `dispatch_string_prelude_assoc_fn` rewrites a `String::from_utf8(v)` call into a free-function call against a prelude function. Generalize this so additional method/assoc names register themselves into a small dispatch table consulted before falling through to the builtin registry.

**Body sketches.** The pure delegations (10 methods) compile to one-liners:

```gruel
fn String__len(s: Ref(String)) -> usize        { s.bytes.len() }
fn String__capacity(s: Ref(String)) -> usize    { s.bytes.capacity() }
fn String__is_empty(s: Ref(String)) -> bool     { s.bytes.is_empty() }
fn String__clear(s: MutRef(String))             { s.bytes.clear() }
fn String__reserve(s: MutRef(String), n: usize) { s.bytes.reserve(n) }
fn String__push_byte(s: MutRef(String), b: u8)  { checked { s.bytes.push(b) } }
fn String__into_bytes(s: String) -> Vec(u8)     { s.bytes }   // partial-move; verify ADR-0036 allows it
fn String__new() -> String                      { String { bytes: Vec(u8)::new() } }
fn String__with_capacity(n: usize) -> String    { String { bytes: Vec(u8)::with_capacity(n) } }
fn String__clone(s: Ref(String)) -> String      { String { bytes: s.bytes.clone() } }
```

The algorithmic methods (4 methods) become small loops or compositions:

```gruel
fn String__concat(a: Ref(String), b: Ref(String)) -> String { /* alloc + push_str twice */ }
fn String__push_str(s: MutRef(String), other: Ref(String)) { /* loop b in other.bytes; s.bytes.push(b) */ }
fn String__contains(haystack: Ref(String), needle: Ref(String)) -> bool { /* byte search */ }
fn String__starts_with(s: Ref(String), prefix: Ref(String)) -> bool { /* byte-prefix check */ }
fn String__ends_with(s: Ref(String), suffix: Ref(String)) -> bool { /* byte-suffix check */ }
fn String__push(s: MutRef(String), c: char) { /* @encode_utf8 via existing char__encode_utf8 + push_byte loop */ }
fn String__from_char(c: char) -> String { /* with_capacity(4) + push */ }
```

**Operators (`==`, `<`, etc.).** These are not method calls — they're operator overloads dispatched in sema before lookup. Two paths:

(a) **Stay in Rust** for now. `__gruel_str_eq`/`__gruel_str_cmp` keep their callers; this is ~80 Rust LOC retained. Cheapest path; deferrable.

(b) **Rewrite as `Vec(u8)` operator inheritance.** Once `String`'s ops dispatch to `self.bytes`'s ops (i.e. equality of two `Vec(u8)` values is byte-string equality), the `__gruel_str_*` helpers go away. Requires `Vec(u8)` to grow ordering operators. Defer to a follow-up ADR; not in this scope.

This ADR picks (a). Operator routing is the last 80 LOC of `string.rs` we don't try to evict.

**What stays in `string.rs` after this ADR:**

- `__gruel_str_eq`, `__gruel_str_cmp` (~80 LOC)
- `__gruel_alloc`/`__gruel_free`/`__gruel_realloc` family (~70 LOC) — used by Vec, not String-specific; could move to `heap.rs` but that's a separate cleanup
- `__gruel_string_alloc`/`__gruel_string_realloc`/`__gruel_string_clone` (~50 LOC) — also Vec-shared byte-buffer helpers; same note
- `__gruel_utf8_validate` (~30 LOC, called from prelude `String__from_utf8`) — must stay; algorithm is portable Gruel but UTF-8 lookup tables and SIMD futures land here

Estimate: ~230 LOC retained, **~520 LOC eliminated** from `gruel-runtime/src/string.rs`, plus **~220 LOC eliminated** from `gruel-builtins/src/lib.rs` (the methods and assoc-fns whose entries become unnecessary).

### Shift 3: Stretch — eliminate the `STRING_TYPE` synthetic-struct entry entirely

Once Shift 2 lands, `STRING_TYPE` in `gruel-builtins` is a stub: a name, one `bytes: Vec(u8)` field, a drop-fn pointer, and the operator entries. The path of greatest LOC reduction is to remove the entry entirely:

- Define `String` as a regular `pub struct String { bytes: Vec(u8) }` in `std/string.gruel` (or in the prelude).
- The lexer/parser already type string literals as `String` by name lookup (no special-casing — confirm during implementation; if there is special-casing in `gruel-air/src/sema/analysis.rs` or `gruel-rir`, route it through name resolution against the prelude/stdlib instead).
- Drop glue: ADR-0010's auto-synthesized drop runs the field's destructor. `Vec(u8)`'s drop already exists. `__gruel_drop_String` becomes unnecessary; the codegen call site at `crates/gruel-codegen-llvm/src/codegen.rs:936` becomes a regular drop emission.
- Field privacy: `bytes` is private (no `pub`); prelude/stdlib code accesses it via the ADR-0073 privileged carve-out (same mechanism that lets the inline prelude touch `Vec(u8)` internals today).

This shift is gated behind Shift 2 because the runtime-FFI layer it depends on must already be Gruel-resident. It is presented here for completeness but split into a final phase that can be deferred if it surfaces hidden coupling.

If Shift 3 lands, **another ~280 LOC disappears** from `gruel-builtins/src/lib.rs`, the `String__*` ad-hoc dispatch table in `analysis.rs` (~60 LOC) collapses, and the codegen drop branch (~20 LOC) simplifies.

### Net Rust-LOC budget

| Phase | Rust LOC removed | Rust LOC added | Gruel LOC added |
|------|---------|---------|---------|
| 1. Prelude on disk | ~225 (string literal) | ~30 (loader + fallback) | ~225 (file move) |
| 2. String methods → Gruel | ~520 (runtime) + ~220 (registry) | ~10 (dispatch) | ~150 |
| 3. Eliminate `STRING_TYPE` | ~280 (registry) + ~80 (sema/codegen branches) | ~5 | ~30 |
| **Total** | **~1325** | **~45** | **~405** |

Rough order of magnitude: **~1.3K Rust LOC removed, ~400 Gruel LOC added**, with no new language features required.

## Implementation Phases

Each phase is independently shippable, ends with `make test` green, and quotes its own LOC delta in the commit message so the running total is auditable.

### Phase 1: Prelude on disk (preview-gated `stdlib_consolidation`)

- [ ] Add `std/_prelude.gruel` containing the current `PRELUDE_SOURCE` content verbatim (modulo whitespace cleanup).
- [ ] Add `PRELUDE_FALLBACK: &str` constant in `unit.rs` mirroring the file (for tests + missing-stdlib robustness).
- [ ] Modify `CompilationUnit::parse()` to attempt loading `_prelude.gruel` via the existing stdlib resolution path (`GRUEL_STD_PATH`, then `std/_prelude.gruel` relative to source). On miss, fall back to `PRELUDE_FALLBACK`.
- [ ] Verify `FileId::PRELUDE` is still assigned regardless of source.
- [ ] Confirm test fixtures that construct a `Sema` directly (`crates/gruel-air/src/sema/tests.rs`, `crates/gruel-air/src/sema/conformance.rs`) still get the prelude — they currently call `inject_builtin_types` but don't load the prelude string; verify whether they need it and adjust.
- [ ] Delete the `PRELUDE_SOURCE` constant once the file path is verified working.
- [ ] No spec-test changes expected. UI tests continue to pass.

### Phase 2a: Pure-delegation String methods → prelude functions

- [ ] Generalize `dispatch_string_prelude_assoc_fn` (`analysis.rs:4731`) into a small table mapping `String::name` and `String__name` to prelude function symbols. (The assoc-fn dispatch already exists; extend it to method dispatch.)
- [ ] Move the 10 pure-delegation methods/assoc-fns (`new`, `with_capacity`, `len`, `capacity`, `is_empty`, `clear`, `reserve`, `push_byte`, `into_bytes`, `clone`, `bytes_len`, `bytes_capacity`) into `std/_prelude.gruel`.
- [ ] Delete the corresponding entries from `STRING_TYPE` in `gruel-builtins/src/lib.rs`.
- [ ] Delete the corresponding `String__*` extern functions from `gruel-runtime/src/string.rs`.
- [ ] Run `make test`. Spec tests for these methods (in `crates/gruel-spec/cases/`) should still pass — the dispatch path is the only thing that changed.
- [ ] Commit-message LOC delta: target `-300` net Rust LOC.

### Phase 2b: Algorithmic String methods → prelude

- [ ] Move `concat`, `push_str`, `contains`, `starts_with`, `ends_with`, `push(c: char)`, `from_char`. Each becomes a small Gruel function operating on `self.bytes`.
- [ ] `push(c: char)` reuses the existing prelude `char__encode_utf8` — already in the prelude, so the call is in-namespace.
- [ ] Delete corresponding registry entries and `String__*` extern functions.
- [ ] Verify codegen quality on `String::len` and `String::push_str` is unchanged (LLVM IR `--emit asm` spot-check: the bytes loaded should be identical to today's, just through one extra inlined function).
- [ ] Commit-message LOC delta: target `-400` net Rust LOC.

### Phase 3: Eliminate `STRING_TYPE` synthetic struct (stretch)

- [ ] Audit: grep for `is_builtin_string`, `builtin_string_id`, `STRING_TYPE`, `String__` across `crates/`. Map every Rust call site that special-cases `String`.
- [ ] Define `pub struct String { bytes: Vec(u8) }` in `std/_prelude.gruel` (private field — relies on ADR-0073 privileged-access carve-out for the prelude file).
- [ ] Route string-literal type assignment through name resolution against the prelude rather than the special `builtin_string_id`.
- [ ] Remove `STRING_TYPE` from `BUILTIN_TYPES`.
- [ ] Remove `__gruel_drop_String` declaration in codegen — the auto-synthesized drop pipeline already handles structs with droppable fields.
- [ ] Delete the special-case operator-routing branch (or leave the registry-based operator overload mechanism intact, attached to the Gruel-defined `String` struct via a small annotation — TBD during implementation).
- [ ] Run `make test`. This phase is the riskiest — landed last.
- [ ] If too much hidden coupling surfaces, **stop after 2b** and ship a follow-up ADR. The Rust-LOC win from 1+2 alone is ~720 lines; that is already worth shipping.

### Phase 4: Stabilization

- [ ] Remove the `stdlib_consolidation` preview gate. (No user-visible feature; the gate exists only to avoid changing behavior mid-flight on long-running branches.)
- [ ] Update ADR status to Implemented.
- [ ] Sweep generated docs (`make gen-intrinsic-docs` or equivalent) — confirm nothing references the deleted `String__*` runtime symbols.

## Consequences

### Positive

- **~1.3K Rust LOC removed**, replaced by ~400 LOC of Gruel that's easier to read and modify.
- **Prelude becomes editable as a normal source file** — syntax highlighting, line-level diffs, no escaping.
- **String/Vec consistency by composition.** ADR-0072's structural promise becomes load-bearing: a change to `Vec(u8)` automatically reaches `String`, instead of requiring twin updates.
- **Stdlib gains its first non-trivial citizen.** `std/string.gruel` (or the prelude file) demonstrates that the on-disk stdlib is a real place to add code, not just a stub for `math`.
- **Lower contributor barrier.** Adding a String method becomes "edit a Gruel file" instead of "edit three Rust files and hope the symbol naming convention is right."

### Negative

- **One indirection at codegen.** `String::len` becomes an inlined call into a Gruel free function. LLVM eliminates this in optimized builds, but `--release`-without-LTO and debug builds may show a single extra call frame in stack traces. Acceptable; matches every other Gruel-defined method.
- **Privileged-access carve-out gets more load-bearing.** ADR-0073's "prelude/stdlib can read non-pub builtin fields" mechanism now governs more code paths. If the carve-out has bugs, more things break. Mitigated by Phase 1 landing first and exercising the mechanism with the same content currently inlined.
- **Prelude file must always be findable.** `GRUEL_STD_PATH` and the `std/`-relative search must be resilient. The `PRELUDE_FALLBACK` constant + tests guard this; a future installer/distribution story is on the wishlist either way.

### Neutral

- **Runtime `__gruel_str_eq`/`__gruel_str_cmp` survive.** Operator routing is left for a future ADR; this one is about deletion volume, not perfection.
- **No spec changes.** `String`'s observable surface is unchanged.
- **No new feature flags surface to users** — `stdlib_consolidation` exists only for internal staging.

## Open Questions

1. **Should `_prelude.gruel` live under `std/` or a sibling `prelude/`?** This ADR picks `std/` for resolution-path reuse. Alternative: keep prelude resolution distinct so a user replacing `std/` for a freestanding target doesn't accidentally lose the prelude. Resolve during Phase 1 implementation.
2. **`String__into_bytes` consuming method.** ADR-0036 banned partial moves out of structs. Verify whether `s.bytes` (where `s: String` is consumed) is still expressible — if not, the prelude version needs a different formulation (e.g. `Vec(u8)::from_string(s)` that does the move via a privileged intrinsic). Not a blocker; surface during Phase 2a.
3. **Algorithmic `contains`/`starts_with`/`ends_with` on `Vec(u8)`.** If `Vec(u8)` doesn't yet expose byte-search methods, the prelude versions iterate manually. Cleaner: add `Vec(u8)::contains(needle: Slice(u8))` and friends in a sibling cleanup. Out of scope here; either path works.
4. **Test fixtures that bypass `CompilationUnit`.** Several `Sema`-direct tests in `gruel-air` may not currently load the prelude. Phase 1 has to verify this and either route them through the same loader or document why they don't need the prelude.

## Future Work

- **Move `Vec(T)` registry entries to Gruel.** Same playbook: `Vec(T)` has a ~100 LOC codegen-method-lowering pass plus a registry stub. If `Vec`'s methods can be expressed as inline Gruel calling raw-pointer + alloc intrinsics, another ~150 Rust LOC goes.
- **Operator routing for stdlib types.** Once string equality is `bytes_eq` over `Vec(u8)`, the `__gruel_str_*` helpers retire. Requires a small operator-on-stdlib-type mechanism; potentially uses the existing built-in interface infrastructure.
- **`std/io`, `std/process`, `std/env`.** With the stdlib mechanism warm, the obvious next surfaces are I/O and process — both currently exist as raw intrinsics in `gruel-runtime` with no nice wrapper.
- **`std/collections`.** `Vec` and a future `HashMap`/`BTreeMap` belong here.

## References

- [ADR-0010: Destructors](0010-destructors.md) — Auto-synthesized drop glue (relied on for Phase 3)
- [ADR-0020: Built-in Types as Synthetic Structs](0020-builtin-types-as-structs.md) — Original synthetic-struct mechanism this ADR partly retreats from
- [ADR-0026: Module System](0026-module-system.md) — Stdlib resolution mechanism (`@import("std")`, `_foo.gruel` directory modules)
- [ADR-0036: Destructuring and Partial-Move Ban](0036-destructuring-and-partial-move-ban.md) — Constrains the `into_bytes` formulation
- [ADR-0050: Centralized Intrinsics Registry](0050-intrinsics-crate.md) — Pattern model: hardcoded enum + registry, drop entries to relocate behavior
- [ADR-0065: Clone and Option](0065-clone-and-option.md) — Established the "Gruel-resident generic enum" prelude pattern
- [ADR-0070: Result Type](0070-result-type.md) — Same pattern, expanded
- [ADR-0071: char Type](0071-char-type.md) — Established the "prelude functions for built-in scalar methods" pattern (`char__encode_utf8`)
- [ADR-0072: String as Vec(u8) Newtype](0072-string-vec-u8-relationship.md) — Direct precursor; this ADR finishes the runtime collapse it summarized but did not complete
- [ADR-0073: Field/Method Visibility](0073-field-method-visibility.md) — Privileged-access carve-out for prelude/stdlib code

---
id: 0069
title: Layout Abstraction and Niche-Filling for Enums
status: proposal
tags: [layout, types, optimization, codegen, internals]
feature-flag: enum-niches
created: 2026-05-01
accepted:
implemented:
spec-sections: ["3.8"]
superseded-by:
---

# ADR-0069: Layout Abstraction and Niche-Filling for Enums

## Status

Proposal

## Summary

Introduce a unified `Layout { size, align, niches }` abstraction in `gruel-air`, route every existing ad-hoc layout computation through it, and use the resulting niche infrastructure to optimize enum representations. The two niche-bearing types available in Gruel today are `bool` (values ≥ 2 are forbidden) and unit-only enums with N < discriminant-max variants (values ≥ N are forbidden). Enums whose only unit variant can be encoded in such a niche elide the discriminant byte entirely. Concretely: `Option(bool)` becomes 1 byte, `Option(SmallEnum)` becomes 1 byte, and nested cases like `Option(Option(bool))` collapse instead of growing. The user-visible wins are modest; the load-bearing change is the layout abstraction itself, which unblocks any future optimization (`NonNull(T)`, lifetime-relaxed `Ref(T)` storage, packed structs, FFI repr attributes) without another ground-up refactor.

## Context

### What this ADR is responding to

ADR-0065 (Phase: "Layout") explicitly deferred `Option` layout optimizations: *"`Option(Ptr(T))` could elide the discriminant by using null-pointer-as-None, but layout optimizations are deferred. v1 ships the naive `{ tag, payload }` layout."* (line 77). That deferral was correct at the time — the substrate wasn't there. This ADR builds the substrate.

### Why we're not doing the Rust-comprehensive version

The obvious first beneficiary in other languages is `Option(&T)` / `Option(*const T)`. In Gruel today, neither path is available:

- **`Ref(T)` / `MutRef(T)`** (ADR-0062) cannot be stored in struct fields, returned, or escape (line 77 of ADR-0062). An `Option(Ref(T))` would require storing `&x` in the enum's payload field, then potentially returning the `Option` — both forbidden. Sema rejects the type today; the optimization is moot until a future "stored references with lifetimes" ADR lifts the rule.
- **`Ptr(T)` / `MutPtr(T)`** (ADR-0028 / 0063) are nullable by design — `Ptr(T)::null()` is a valid value, so null is *not* a forbidden bit pattern and cannot serve as a niche. A separate `NonNull(T)` type would unlock the optimization but is a real type-system addition, out of scope here.

So the Rust-style pointer-niche win is unavailable until one of those changes lands. What *is* available — `bool` and small unit enums — has a small immediate payoff but exercises every piece of machinery we'll need later. Doing the substrate work now means the future `NonNull(T)` or stored-refs ADRs become "declare a niche on the new type" rather than "build the whole optimization framework."

### Survey of what exists today

- **No `Layout` struct.** Size and alignment are recomputed ad-hoc in `crates/gruel-codegen-llvm/src/types.rs` (`type_byte_size` lines 101–131, `type_alignment` lines 38–46). Enum payload offsets are computed inline at the constructor site (`crates/gruel-codegen-llvm/src/codegen.rs` ~lines 2537–2573) and again at the extraction site (~lines 2603–2607). There is no single source of truth, and no place for niche metadata to live.
- **Enum discriminant selection** lives in `gruel-air`: `EnumDef::discriminant_type()` (`crates/gruel-air/src/types.rs:818–833`) picks the smallest of `{u8, u16, u32, u64}` that holds the variant count. This is a clean point to teach about niches.
- **Match-switch lowering** assumes "discriminant is field 0" of the enum struct (`codegen.rs:~5128–5162`). Niche-encoded enums need this site to consult layout instead.
- **`bool` is `i1` in registers** but enum payloads use a `[N x i8]` byte array with unaligned stores (`codegen.rs:2571: store.set_alignment(1)`). The niche on `bool` lives at the byte level (values 2..=255 of the storage byte).
- **`char`** does not exist. **Function pointer types** do not exist. **`Ptr(T)`** is nullable. **`Ref(T)`** cannot be stored. So `bool` and small unit enums are genuinely the entire candidate niche-bearer set today.

### Why bundle the refactor with the optimization

The refactor alone (Layout struct, route everything through it) has no observable user benefit and would feel like make-work. Bundling the smallest user-visible optimization (1-byte `Option(bool)`, collapsing nested Options) gives the refactor a real test: the new abstraction must be expressive enough to drive correct codegen for two distinct layout shapes. Without that exercise, we'd ship a Layout struct that happens to be ready for niches but has never actually carried them — and discover the gaps later.

## Decision

Two parts: a layout abstraction (Part 1), and the niche optimization built on top (Part 2).

### Part 1 — `Layout` abstraction

A new type lives in `crates/gruel-air/src/layout.rs`:

```rust
pub struct Layout {
    pub size: u64,
    pub align: u64,
    pub niches: SmallVec<[NicheRange; 1]>,
}

pub struct NicheRange {
    /// Byte offset within the type where the niche-bearing bytes live.
    pub offset: u32,
    /// Width of the niche-bearing region, in bytes.
    pub width: u8,
    /// Inclusive forbidden range, interpreted as a little-endian unsigned
    /// integer of `width` bytes. Reading these bytes from a valid value
    /// of the type will never produce a value in `[start, end]`.
    pub start: u128,
    pub end: u128,
}
```

A single query:

```rust
pub fn layout_of(pool: &TypeInternPool, ty: TypeId) -> Layout;
```

Layouts are pure functions of the type (types are interned), so the result is cached on the `TypeInternPool` after first computation. Cache key is `TypeId`; there is no invalidation.

All existing layout computations — `type_byte_size`, `type_alignment`, enum constructor offset math, match-extract offset math, drop-glue field walking — migrate to consult `layout_of`. After migration, `gruel-codegen-llvm/src/types.rs` is a thin adapter that calls `layout_of` and pulls `size` / `align` from it.

#### Niche population in `layout_of`

| Type | Niches (in v1) |
|------|----------------|
| `bool` (1-byte storage) | `{ offset: 0, width: 1, start: 2, end: 255 }` |
| `i8`/`u8`/`i16`/.../`i64`/`u64` | none — every bit pattern is valid |
| `f32`/`f64` | none in v1 (NaN payloads are valid; we don't claim a signaling-NaN niche) |
| `Ptr(T)` / `MutPtr(T)` | none (nullable) |
| `Ref(T)` / `MutRef(T)` | none in stored position (the type can't appear there anyway) |
| Struct `{ f1, f2, ... }` | inherit each field's niches, with `offset` adjusted to the field's offset within the struct |
| Unit-only enum with N variants and discriminant width W | `{ offset: 0, width: W, start: N, end: max_for_width(W) }` |
| Enum with data variants (pre-niche layout) | none (the discriminant slot occupies a fixed position; future work could expose niches in the unused upper bits, but not in v1) |
| `Option(T)` and other two-variant enums | see Part 2 — may inherit the payload's niche |

### Part 2 — Niche-filled enum layout

An enum is a candidate for niche encoding when *all* of the following hold:

1. It has exactly one unit variant.
2. It has at least one data variant. (For Option, exactly one — see "Generalization" below for what's deferred.)
3. The data variant's payload type has a `NicheRange` with at least one usable value the unit variant can claim.

For such an enum, `layout_of` returns a layout with:

- `size = layout_of(payload).size` (no separate discriminant byte).
- `align = layout_of(payload).align`.
- The unit variant is encoded by writing `niche.start` (a single chosen forbidden value) at `niche.offset` within the enum's storage. This consumes one value from the niche range; the remaining range `[start+1, end]` is exposed as the enum's own niche, so the optimization composes (`Option(Option(bool))` collapses to 1 byte: `None_outer` = 2, `Some(None_inner)` = 3, `Some(Some(false))` = 0, `Some(Some(true))` = 1).

For enums that don't qualify (multiple unit variants, multiple data variants, payload with no niche), layout falls back to today's `{ discriminant, [max_payload x i8] }` shape unchanged.

#### Codegen consequences

| Operation | Pre-niche | Niche-encoded |
|-----------|-----------|---------------|
| Construct unit variant | store discriminant N at field 0 | store `niche.start` at byte offset `niche.offset` (no discriminant) |
| Construct data variant | store discriminant 0 at field 0; store payload bytes at offset 1 (or after discriminant) | store payload bytes at offset 0 (no discriminant) |
| Match-extract discriminant | `extract_value field 0` | load `width` bytes at `niche.offset`; the value tells you which variant: in the niche range → unit variant, otherwise → data variant |
| Pattern bind payload | extract from payload byte array | the payload occupies the entire enum storage; no offset shift |

The codegen change lives in `crates/gruel-codegen-llvm/src/codegen.rs` at the constructor (~lines 2504–2583) and the match dispatch (~lines 5128–5162). Both sites switch from hardcoded "field 0 / payload at offset 1" to "consult `Layout` for the discriminant strategy."

#### What the optimization buys today

| Type | Pre-ADR size | Post-ADR size |
|------|--------------|---------------|
| `Option(i32)` | 8 bytes | 8 bytes (no payload niche) |
| `Option(bool)` | 2 bytes | **1 byte** |
| `Option(Option(bool))` | 4 bytes (2-byte tag + 2-byte payload aligned) | **1 byte** |
| `Option(Color)` where `enum Color { R, G, B }` | 2 bytes | **1 byte** |
| `Option(Ptr(T))` | 16 bytes | 16 bytes (Ptr is nullable) |
| `Option(Ref(T))` | (rejected by sema) | (rejected by sema) |

The wins are real but scoped. The infrastructure is the durable artifact.

### Generalization deferred to future ADRs

These are deliberately *not* in v1:

- **Multi-data-variant niche packing** (`enum E { A, B(i32), C(i32) }` packing A into the unused tag values of a shared discriminant). The framework allows it, but the layout algorithm and codegen for "discriminant lives in payload bits with non-trivial mapping" is materially more complex than the Option-shaped case.
- **Multiple unit variants sharing a payload niche** (`enum E { A, B, Some(bool) }` putting A=2, B=3 into bool's niche). Same reason — needs a "claim N consecutive niche values" extension to `NicheRange` and a more general construct/match codegen.
- **`NonNull(T)` type** — separate ADR; would add a single line to the niche table once it exists.
- **Enum-data niche exposure** — even the post-niche `Option(bool)` (1 byte, niche `{3..=255}`) could itself be a niche bearer for a further enclosing enum; this works correctly via the inherited-niches rule, but propagating niches *out of* general data-carrying enums requires payload-niche tracking that v1 doesn't do.
- **Float niches** (signaling NaNs).
- **Layout attributes** (`@layout(c)`, `@layout(packed)`) — the framework will need to grow opt-outs eventually, but no in-tree caller needs them yet.

## Implementation Phases

Each phase is independently committable and testable.

- [x] **Phase 1: `Layout` struct, computation, cache (no behavior change).** Add `crates/gruel-air/src/layout.rs` with the `Layout` and `NicheRange` types and `layout_of(pool, ty)`. Compute size/align using the same rules `type_byte_size` / `type_alignment` use today; leave `niches` empty for every type. Cache on `TypeInternPool`. No callers yet; verify with unit tests that `layout_of` agrees with the existing functions across every type kind.

- [x] **Phase 2: Migrate codegen size/align queries to `Layout`.** `type_byte_size` and `type_alignment` in `gruel-codegen-llvm/src/types.rs` become thin wrappers over `layout_of`. All other in-tree callers go through `Layout`. Existing test suite (full `make test`) must pass unchanged — no observable behavior should differ.

- [x] **Phase 3: Migrate enum constructor and match-extract through `Layout`.** Add `Layout::discriminant_strategy()` returning either `Separate { offset, width }` (current behavior) or `Niche { ... }` (Phase 5+). Constructor and match-switch lowering in `codegen.rs` consult this instead of hardcoding field 0. Phase 3 still produces only `Separate`, so wire format and tests remain identical.

- [x] **Phase 4: Populate niches for `bool` and unit-only enums.** Extend `layout_of` to fill in `niches` for `bool` (`{2..=255}` at offset 0) and unit-only enums (unused tag values). Pure data; no codegen change. Add unit tests asserting niche presence.

- [x] **Phase 5: Niche-aware layout for two-variant Option-shaped enums.** When laying out an enum with one unit variant and one data variant whose payload has a usable niche, return a `Layout` with the niche-encoded shape (no discriminant byte; size = payload size; remaining niche values exposed). Gate behind `--preview enum-niches` in `gruel-air` so Phase 5+ work can land incrementally without affecting non-preview compilations.

- [x] **Phase 6: Codegen for niche-encoded enums.** Constructor stores payload directly + writes `niche.start` for the unit variant. Match-dispatch loads the niche bytes and tests range membership instead of equality on a tag. Pattern binding for the data variant reads the payload from offset 0. End-to-end tests behind the preview gate: `Option(bool)` size and round-trip, nested `Option(Option(bool))`, `Option(SmallEnum)`.

- [ ] **Phase 7: Composability and recursive niche inheritance.** Niche-encoded enums expose their *remaining* niche range as their own `Layout::niches`, so they can be re-niched by an enclosing enum. Tests for `Option(Option(Option(bool)))` collapsing to 1 byte; for `Option` of a struct containing a `bool` (niche inherited via struct field offset).

- [ ] **Phase 8: Spec.** Add a section to `docs/spec/src/03-types/` documenting layout guarantees: types have a defined size/alignment, but specific representations (presence/absence of a discriminant, where it lives) are implementation choices except where explicitly guaranteed (e.g., `repr(C)` if/when it lands). Note that pattern matching, equality, and field access are the only stable observables; raw bit pattern inspection of an enum is not. Update the generated builtins reference if `Option`'s documented size table needs revisions.

- [ ] **Phase 9: Stabilize.** Remove the `enum-niches` preview gate, drop `PreviewFeature::EnumNiches`, update ADR status to `implemented`. Confirm full `make test` passes without `--preview`.

### Preview gating

A preview gate (`--preview enum-niches`) is added in Phase 5 only because the optimization changes observable sizes (via `size_of`, FFI struct interop, future intrinsics). The gate lets the optimization land progressively without disturbing existing programs that may have inadvertently encoded a 2-byte assumption about `Option(bool)`. By Phase 9 the gate is removed; programs depending on the old layout were depending on undocumented behavior.

`PreviewFeature::EnumNiches` is added to `crates/gruel-util/src/error.rs` (the file CLAUDE.md calls `gruel-error/src/lib.rs`; the actual location is `gruel-util`) per the standard ADR-0005 protocol.

## Consequences

### Positive

- **Layout becomes a first-class concept.** Future ADRs (`NonNull(T)`, lifetime-aware `Ref` storage, packed structs, repr attributes, FFI alignment) attach to a coherent abstraction instead of duplicating ad-hoc logic.
- **`Option(bool)` and similar collapse.** A small but real density win for any code holding `Option(bool)` flags (parser state, interpreter state, etc.). Nested-`Option` collapse is occasionally meaningful in generic code that accidentally double-wraps.
- **`Option(SmallEnum)` becomes free.** State machines and tag types that wrap user enums in `Option` no longer pay a separate discriminant byte.
- **Pattern matching unchanged at AIR level.** All the work is below the AIR boundary; sema, the pattern checker, and the borrow checker are untouched. Only `gruel-codegen-llvm` learns the new shape.
- **Sets up the easy future wins.** Once `NonNull(T)` exists, declaring its niche is a one-liner; `Option(NonNull(T))` then collapses with no further codegen work.

### Negative

- **Refactor risk dominates the effort.** Phase 1–3 is mechanical but touches every place that asks for a type's size or alignment. Bugs here surface as miscompiles, not as type errors, so testing has to be thorough (golden tests on AIR/CFG layout output, plus runtime spec tests covering size and round-trip for many shapes).
- **Codegen for niche-encoded enums is a divergent path.** The constructor and match-dispatch now have two shapes (separate-discriminant vs. niche-encoded), increasing surface area in `codegen.rs`. The `discriminant_strategy()` abstraction keeps the divergence localized but not eliminated.
- **Observable size changes can break programs that reach for raw bytes.** Anyone using `@transmute` or FFI to peek at an `Option(bool)` will see a different layout. The preview gate buys a soft landing; the spec section makes the guarantee surface clear; but it is still a breakage class to be aware of.
- **The user-visible payoff is small.** Without `NonNull` or stored-refs, no pointer-sized `Option` shows up. A reader skimming the changelog will see "1-byte `Option(bool)`" and reasonably wonder if it was worth the work. The case for doing it now is the substrate, not the size table.
- **Layout cache lives forever on the type pool.** Memory is bounded by the number of interned types, but it is a new persistent allocation. Likely negligible in practice; called out for honesty.

### Neutral

- **No new IR instructions, no new sema concepts, no new spec rules beyond clarifying what's not guaranteed.** The change is structural rather than semantic.
- **`f32` / `f64` get no niches in v1.** Reserving signaling-NaN bit patterns is a known technique but adds platform/ABI complexity disproportionate to the payoff. Punt.

## Open Questions

1. **Should `Option(bool)` and similar collapses be guaranteed by the spec, or merely permitted?** Tentative: **permitted, not guaranteed.** Guaranteeing locks future codegen choices. Permitted means the optimizer can do it (and currently does), but a future revision is free to switch strategies (e.g., for cache-line packing reasons).

2. **How should the preview gate behave at the type-system boundary?** When `--preview enum-niches` is off, `layout_of(Option(bool))` returns the 2-byte layout; when on, the 1-byte layout. Mixing crates compiled with and without the gate is theoretically incoherent. Tentative: the gate is per-compilation-unit (matches the existing preview-feature model). The compiler does not link mixed-preview object files; this is fine because Gruel does not yet have stable cross-crate ABI.

3. **Should `Layout` track field offsets for structs as well as size/align/niches?** Today, struct field offsets are computed in two places (sema, codegen) and they agree by parallel construction. Folding field-offset computation into `Layout` would centralize a third concern. **Tentative: yes, but in a follow-up ADR** — keep this ADR scoped to size/align/niches; the field-offset move is a refactor with its own test surface.

4. **What about `Slice(T)` and `MutSlice(T)`?** They are `{ ptr, len }` fat pointers. Their `ptr` field is non-null per construction (slices always point to live memory). A future "stored references" ADR would make this a niche; today, the same non-storage rules apply, so it's moot.

5. **Should the niche-encoded layout extend to enums with one unit and *multiple* data variants** (e.g., `enum E { Empty, A(i32), B(i32) }` putting `Empty` in a niche of A's i32, with a separate flag distinguishing A from B)? This requires a real discriminant for A-vs-B, defeating the niche win. Tentative: **no** — Option-shaped (one unit + one data) is the cleanly-specified case; richer variants stay on the separate-discriminant path until a more general layout algorithm arrives.

6. **Do we want `layout_of` queries to be available to user programs via an intrinsic** (e.g., `@niche_count(T)`)? Tentative: **no.** Layouts are an internal optimization; exposing them to user code creates a backwards-compat surface we don't want. `size_of` and `align_of` remain the only stable layout observables.

## Future Work

- **`NonNull(T)`** — a non-nullable pointer type. Adds null as a niche on `NonNull`, immediately enabling pointer-sized `Option(NonNull(T))`. Separate ADR (it's a real type-system addition).
- **Lifetime-relaxed `Ref(T)` storage** — once `Ref(T)` can live in struct fields and enum payloads, declaring its niche makes `Option(Ref(T))` pointer-sized. Same one-line change.
- **Multi-variant niche packing** — `enum E { A, B, Some(bool) }` packing both A and B into bool's niche range. Generalizes the v1 algorithm.
- **Discriminant-niche exposure** — letting general data-carrying enums expose niches in their unused discriminant bits, so `Option(Result(T, E))` and similar collapse one more layer.
- **Layout attributes** — `@layout(c)`, `@layout(packed)`, `@layout(transparent)` for FFI and ABI control. Will need a layout opt-out mechanism; the `Layout` struct is the natural place to add a `mode` field.
- **Float niches** — claim signaling-NaN bit patterns for `Option(f32)` / `Option(f64)`. Platform-dependent; defer until the payoff is felt.
- **Field-offset centralization** — fold struct field offset computation into `Layout`, eliminating the parallel computation in sema and codegen.

## References

- ADR-0005: Preview Features
- ADR-0020: Built-in Types as Synthetic Structs
- ADR-0028: Pointer Types and Memory Operations
- ADR-0037: Enum Data Variants and Full Pattern Matching
- ADR-0061: Generic Pointer Types
- ADR-0062: Reference Types (defines the `Ref(T)` non-storage rule that blocks the pointer-niche win)
- ADR-0063: Pointer Method Syntax
- ADR-0065: Clone Interface and Canonical `Option(T)` (deferred the optimization this ADR delivers)
- [Rust niche-filling RFC discussion](https://rust-lang.github.io/unsafe-code-guidelines/layout/enums.html)
- [Rust `NonZero*` / `NonNull` types](https://doc.rust-lang.org/std/num/struct.NonZeroU32.html)

---
id: 0051
title: Recursive Pattern Lowering
status: implemented
tags: [patterns, pattern-matching, air, cfg, exhaustiveness, refactor]
feature-flag:
created: 2026-04-23
accepted: 2026-04-23
implemented: 2026-04-23
spec-sections: ["4.7"]
superseded-by:
---

# ADR-0051: Recursive Pattern Lowering

## Status

Implemented

## Summary

Replace the astgen-based pattern elaboration pipeline introduced in ADR-0049
phases 4–5 with a recursive `AirPattern` + cascading-switch CFG lowering —
the "principled path" originally sketched in ADR-0049 §8 under "Alternative
considered". This closes the remaining shape gaps that still panic in
astgen (multi-arm top-level `Struct` / `Tuple` / `Ident`, refutable nested
sub-patterns that don't merge, rest patterns inside merged arms, non-leaf
tuple elements in merged arms), lets the exhaustiveness checker walk the
real user pattern tree so non-exhaustive diagnostics can render witnesses
as nested patterns (`Some(None)`) instead of synthesised inner scrutinees
(`match __refut_0 { ... }`), and removes the growing elaboration layer
whose special cases will otherwise multiply as `if let`, or-patterns, and
range patterns land. User-visible semantics are unchanged; this is a
compiler-internal refactor that completes ADR-0049.

## Context

### What exists

ADR-0049 shipped nested patterns by rewriting the AST before lowering:

- `try_elaborate_irrefutable_match` expands a single-arm top-level
  struct/tuple/ident match into `{ let pat = scr; body }`.
- `try_elaborate_tuple_match` (Phase 5a) expands a tuple-root match into
  `let __match_scr_N = scr;` followed by a reverse-folded if/else chain of
  equality tests and per-arm `let` bindings, with a runtime
  `@panic("non-exhaustive match")` fallback for non-exhaustive shapes.
- `try_elaborate_refutable_nested_match` (Phase 5b) merges arms sharing an
  outer variant key into a single outer arm whose body is a nested match
  over a fresh `__refut_N` scrutinee (tupled field values for multi-field
  variants so Phase 5a's elaborator can then handle the inner match).
- Nested irrefutable sub-patterns in variant fields are replaced with
  synthetic `__nested_pat_N` bindings at pattern lowering time and the
  arm body is wrapped with `let` statements that recursively destructure
  the bound values.

`AirPattern` stays as a flat dispatch tag (`Wildcard | Int | Bool |
EnumVariant { enum_id, variant_index }`), and the CFG's match lowerer
emits a single switch over that tag.

### The remaining gap

The elaboration layer is incomplete in two user-visible ways and one
architectural way:

1. **Unsupported shapes panic.** `crates/gruel-rir/src/astgen.rs` still
   contains five "ADR-0049 Phase 5" panics for:
   - `Pattern::Struct { .. } | Pattern::Tuple { .. } | Pattern::Ident { .. }`
     at the top of a *multi-arm* match (line 2136 — single-arm works).
   - Refutable nested sub-patterns in a tuple element or struct-variant
     field of a Phase 5b merged arm (lines 2000, 2173, 2238).
   - Shared-outer merges that involve `..` rest patterns.
2. **Exhaustiveness witnesses reference synthetic scrutinees.** Because
   the exhaustiveness checker runs on the *elaborated* match, for a
   shared-outer merged arm it reports missing patterns against
   `match __refut_0 { ... }` rather than reconstructing them as
   user-visible nested patterns like `Some(None)`. Phase 8 partially
   closed this by naming specific missing patterns
   (`ErrorKind::NonExhaustiveMatch { missing }`), but only at the outer
   level; nested witnesses still surface as references to synthesised
   locals. ADR-0049 lists this as the last remaining future-work item.
3. **The elaboration layer is a dead end.** Every new pattern feature
   (`if let`, or-patterns, range patterns, guards) will either require
   more special-case elaborators or force the move to recursive lowering
   anyway. Each special case compounds: Phase 5b exists partly because
   Phase 5a can't handle nested refutables inside tuple elements, and
   merged-multi-field arms exist partly because Phase 5b can't dispatch
   on multiple fields directly. The complexity is structural, not
   accidental.

### Why now

ADR-0049 is stabilised (`PreviewFeature::NestedPatterns` removed, spec
tests unconditional), so there is no flag to flip — the fix ships as a
bug-fix-shaped refactor. The elaboration layer is still small enough that
a wholesale replacement is tractable; every additional pattern feature
will make the replacement larger. Doing this before `if let` / or-patterns
means those ADRs build on the recursive infrastructure instead of
adding another elaborator.

### Existing infrastructure we can reuse

- **RIR match patterns** (`RirPattern::DataVariant`, `StructVariant`,
  etc.) already carry per-field `RirPatternBinding`s and retain
  source-level nesting information at the point sema consumes them.
  No RIR changes are required — RIR is already recursive.
- **`RirPattern::Struct` and `RirPattern::Tuple`** already exist for
  let-destructure and carry nested sub-patterns.
- **Sema's `analyze_match`** already resolves variant indices, drops
  non-copy unlisted fields (§Phase 6), and emits per-binding
  `StorageLive` / field extractions. The recursive pattern gets wired
  in by letting analyze_match recurse into each sub-pattern.
- **CFG `Terminator::Switch`** already handles tag dispatch; cascading
  switches compose by chaining switch blocks, which is what the recursive
  lowering emits naturally.
- **Usefulness/exhaustiveness algorithm.** The canonical
  Maranget usefulness algorithm (already referenced by ADR-0049) operates
  on recursive patterns directly. The current flat algorithm is a
  specialisation of it.

## Decision

Replace the two intermediate layers — astgen pattern elaboration and flat
`AirPattern` — with a single recursive `AirPattern` walked by CFG
lowering and by the exhaustiveness checker.

### 1. Recursive `AirPattern`

```rust
pub enum AirPattern {
    /// Matches anything, binds nothing.
    Wildcard,

    /// Binds the scrutinee (or its projection) to a local. `inner` is
    /// the sub-pattern applied to the same value; `None` is a bare
    /// binding (equivalent to Rust's `x`), `Some(p)` is `name @ p`.
    /// `name @ p` is not yet exposed in surface syntax — the AIR shape
    /// carries it because every named-binding path in sema produces
    /// `Bind { inner: None }`, and @-patterns are cheap to slot in later.
    Bind { name: Spur, is_mut: bool, inner: Option<Box<AirPattern>> },

    /// Literal scalars. Match by equality on the scrutinee's value.
    Int(i64),
    Bool(bool),

    /// Tuple dispatch. Arity matches the scrutinee's tuple arity;
    /// each `elems[i]` applies to projection `i`.
    Tuple { elems: Vec<AirPattern> },

    /// Named-struct dispatch. `fields[i] = (field_index, sub_pattern)`;
    /// unlisted fields (originating from `..`) produce an implicit
    /// `Wildcard` entry during sema lowering so the AIR shape is
    /// always a complete cover.
    Struct { struct_id: StructId, fields: Vec<(u32, AirPattern)> },

    /// Enum data-variant dispatch. `fields[i]` applies to positional
    /// field `i` of the variant. Unlisted fields (rest `..`) become
    /// explicit `Wildcard` entries at sema time.
    EnumDataVariant {
        enum_id: EnumId,
        variant_index: u32,
        fields: Vec<AirPattern>,
    },

    /// Enum struct-variant dispatch. Same shape as `Struct` but
    /// tagged by variant.
    EnumStructVariant {
        enum_id: EnumId,
        variant_index: u32,
        fields: Vec<(u32, AirPattern)>,
    },

    /// Retained for unit enum variants (today's `EnumVariant`).
    EnumUnitVariant { enum_id: EnumId, variant_index: u32 },
}
```

Encoding: Nested patterns are flattened into the extra-array with a
`len`-prefixed pre-order walk per pattern. Each node tag gets a fixed
header, followed by its child pattern spans. This is the standard SoA
encoding for recursive IRs already used elsewhere (e.g. RIR instruction
extra arrays), so the cost is encoding/decoding glue, not a new
paradigm.

### 2. Sema: produce recursive AIR patterns directly

`analyze_match` today converts `RirPattern` → `AirPattern` in a flat
switch. That conversion becomes `lower_pattern(&RirPattern) ->
AirPattern` with a recursive body: each sub-pattern slot recurses.
Bindings are introduced at `Bind` leaves via the existing
`StorageLive` / field extraction path, generalised to follow
projections through nested `Tuple` / `Struct` / `EnumDataVariant` /
`EnumStructVariant` levels.

Concretely:

- Flatten the current `RirPattern::Tuple` / `RirPattern::Struct`
  pattern arms produced by let-destructure elaboration *at the arm
  level* in sema, not in astgen: let-destructure still goes through
  `StructDestructure` (which is already efficient), but match arms
  with a top-level tuple / struct pattern get a recursive
  `AirPattern::Tuple` / `Struct` arm instead of being rewritten to
  `{ let pat = scr; body }`.
- Drop `analyze_match`'s use of `__nested_pat_N` synthetic bindings.
  Nested sub-patterns translate directly to recursive `AirPattern`
  nodes, and the arm body receives real named bindings from `Bind`
  leaves.
- `..` rest patterns are already expanded to wildcard bindings during
  the current Phase 6 sema step; keep that expansion and emit the
  extra `AirPattern::Wildcard` entries in the recursive form.

### 3. CFG: recursive cascading-switch lowering

Match lowering becomes a recursive descent over
`(scrutinee_place, AirPattern, arm_body, fallthrough_block)`:

- `Wildcard`: unconditional branch to `arm_body`.
- `Bind { name, inner }`: emit `StorageLive` + store the current
  projection into the named local, then recurse on `inner`
  (treating `None` as `Wildcard`).
- `Int(n)` / `Bool(b)`: emit an `icmp eq` + conditional branch:
  equal → `arm_body`, unequal → `fallthrough_block`.
- `Tuple { elems }`: no dispatch needed (tuples are always the
  scrutinee's own shape). For each element, recurse with the
  projection `scr.i` as the scrutinee; the fallthrough block of
  element `i` becomes the scrutinee + projection block for element
  `i+1`. The leaf element's `arm_body` is the true arm body.
- `Struct { fields }`: same as Tuple but using named field
  projections.
- `EnumDataVariant { variant_index, fields }` /
  `EnumStructVariant { .. }`: emit a switch on the discriminant;
  the matching-variant block then lowers `fields` recursively
  using the variant's projection; other discriminants branch to
  `fallthrough_block`.
- `EnumUnitVariant { variant_index }`: discriminant switch only,
  no field recursion.

Matches are compiled arm-by-arm, each arm producing a
fallthrough-chained subgraph. The final arm's fallthrough is either
a `Terminator::Unreachable` (when sema proved exhaustiveness) or a
`@panic("non-exhaustive match")` call (matching today's Phase 5a
behaviour for the preserved compile-time non-exhaustive diagnostic
path). Consecutive arms that dispatch on the same scrutinee + same
discriminator kind (e.g. all integer literals) can still fall back
to a flat `Terminator::Switch` as a peephole optimisation; this is
a follow-on performance concern, not a correctness one.

### 4. Remove the astgen elaboration layer

Delete or shrink:

- `AstGen::try_elaborate_irrefutable_match`
- `AstGen::try_elaborate_tuple_match`
- `AstGen::try_elaborate_refutable_nested_match`
- `AstGen::gen_match_arm_pattern`'s `__nested_pat_N` synthesis
- `wrap_match_arm_body_with_destructures` and the `nested`
  threading parameter
- The five `panic!("... ADR-0049 Phase 5 ...")` sites in
  `gruel-rir/src/astgen.rs`
- The `@panic("non-exhaustive match")` injection currently done by
  `try_elaborate_tuple_match`; the CFG now owns the fallthrough
  terminator

Let-destructure stays on `emit_let_destructure_into` and
`StructDestructure`. Irrefutable let patterns have a simpler shape
and no dispatch, so rewriting them into the recursive AIR form
would add encoding overhead without benefit. The recursive pattern
infrastructure is match-specific.

### 5. Recursive usefulness / exhaustiveness

Replace the current specialised exhaustiveness check for variants +
literals with the canonical usefulness algorithm (Maranget 2007),
operating directly on recursive `AirPattern`s:

- Maintain a matrix `P` where each row is the pattern vector of an
  arm (initially length 1).
- `U(P, q)`: `q` is useful w.r.t. `P` iff at least one concrete
  value matches `q` and no row of `P`. Specialisation on the head
  constructor recurses into sub-patterns; the standard rules for
  `Wildcard` and `Bind` treat them as column-agnostic.
- Non-exhaustiveness witnesses: run `U(P, _)` against a pattern of
  wildcards matching the scrutinee's type; the set of concrete
  counter-examples returned is the missing witness list.

### 6. Nested diagnostic witnesses

`ErrorKind::NonExhaustiveMatch { missing: Vec<String> }` keeps its
shape; the renderer just receives fully-formed nested pattern
strings from the usefulness algorithm's witnesses. E.g. for
`match opt { Some(Some(v)) => ..., None => ... }` the witness set
is `{ Some(None) }` and the error reads:

```
error: non-exhaustive patterns
  --> 3:5
   |
 3 |     match opt {
   |     ^^^^^ pattern `Some(None)` not covered
```

Witness rendering reuses the `AirPattern` Display impl (to be
updated in Phase 1) plus the `EnumDef` / `StructDef` metadata
already in `SemaCtx`.

### 7. What user-visible semantics change

Nothing. Exhaustiveness rules, drop rules, binding rules, and
refutability rules are unchanged from ADR-0049. This ADR is an
internal refactor plus completion of deferred shapes. The three
concrete user-visible differences are:

- Programs that previously hit an "ADR-0049 Phase 5" panic now
  compile.
- Non-exhaustive match diagnostics render nested witnesses.
- Match code size and codegen shape may differ (cascading
  switches vs. if-chains); microbenchmarks are in scope for
  Phase 7.

### 8. No preview gate

ADR-0049 is stabilised; this refactor preserves its user-visible
semantics while extending coverage. A preview flag would gate a
compiler-internal rewrite, not a language feature, so there is no
`PreviewFeature::RecursivePatternLowering`. During rollout the old
elaboration layer stays in place until Phase 4; rollback is "revert
the ADR-0051 commits".

## Implementation Phases

Each phase is independently committable; phases 1–3 introduce the new
lowering alongside the old one so the compiler keeps working
throughout. The switchover is a single edit in `gen_expr`'s match
path (Phase 4).

- [x] **Phase 1: Recursive `AirPattern` + encoding**
  - Extend `AirPattern` in `crates/gruel-air/src/inst.rs` with
    `Tuple`, `Struct`, `EnumDataVariant`, `EnumStructVariant`,
    `EnumUnitVariant`, `Bind` variants. Keep `Wildcard`, `Int`,
    `Bool`.
  - Rewrite `AirPattern::encode` and `MatchArmIterator` to handle
    the recursive extra-array encoding.
  - Display impl renders patterns as the source-level surface
    form (used for witness rendering later).
  - No sema / CFG wiring yet. Unit tests: round-trip encode/decode
    for every shape, including nested.

- [x] **Phase 2: Sema recursive lowering (alongside the old path)**
  - Add `lower_pattern(&RirPattern) -> AirPattern` in
    `gruel-air/src/sema/analyze_ops.rs`, behind a feature-gated
    internal flag (not a user-facing preview flag — an internal
    `SemaCtx` bool set by a `--recursive-pattern-lowering` CLI
    flag, used only for testing).
  - When the flag is on, `analyze_match` emits recursive
    `AirPattern`s; with the flag off it preserves today's flat
    behaviour. Both paths share `analyze_match`'s arm scope / drop /
    field-extraction code.
  - `lower_pattern` handles `Tuple`, `Struct`, nested
    `DataVariant` / `StructVariant`, `Bind`, `Wildcard`, literals.
    Rest patterns continue to expand to wildcards inside
    `lower_pattern`.
  - Unit tests: for each RIR pattern shape, assert the
    produced AIR pattern tree.

- [x] **Phase 3: CFG recursive cascading-switch lowering**
  - Implement `lower_pattern_match(scr_place, pattern, arm_body,
    fallthrough)` in `gruel-cfg/src/build.rs` as a recursive
    descent emitting projection + switch blocks.
  - The flat `AirInstData::Match` lowering stays for the
    non-recursive patterns; the recursive case dispatches to
    the new lowerer.
  - Non-exhaustive fallthrough terminator is chosen by a
    sema-provided `exhaustive: bool` flag on `AirInstData::Match`
    (Phase 2 wires it up): `Unreachable` when exhaustive,
    otherwise an intrinsic call to `@panic("non-exhaustive
    match")`.
  - Spec tests: runtime behaviour of each previously-panicking
    shape (nested `Some(Some(v))`, multi-arm top-level tuple,
    merged arms with `..`, multi-field merged refutables).
    Tests gated on `--recursive-pattern-lowering` until Phase 4.

- [x] **Phase 4: Cut over to the new lowering by default** (except
      `try_elaborate_refutable_nested_match`; see Phase 5 for why)
  - [x] 4a: extend RIR with `Ident` / `Tuple` / `Struct` variants and a
        self-describing tree encoding for nested sub-patterns. Additive;
        astgen still elaborates today.
  - [x] 4b part 1: thread astgen flag + sema wiring for new RIR shapes.
  - [x] 4b part 2: CFG cascading projection + dispatch for top-level
        Tuple / Struct / Bind arms; Bind leaves are currently transparent
        (no storage introduced yet; Phase 4c moves binding introduction
        into CFG).
  - [x] 4c part 1: move binding introduction into sema's recursive
        walker so `(a, 1) => a` style arms resolve end-to-end.
  - [x] 4c part 2: exhaustiveness bookkeeping treats irrefutable
        tuple / struct arm roots as catch-alls.
  - [x] 4c part 3: flip the flag default to on. `try_elaborate_tuple_match`
        no longer runs by default; `try_elaborate_refutable_nested_match`
        still handles `Some(Some(x))` shapes RIR cannot yet represent
        (flat variant bindings), so it stays. Non-exhaustive tuple
        matches are now compile errors (per §Open-Questions-3).
  - [x] 4c part 4: extended `RirPatternBinding` with `sub_pattern` so
        irrefutable nested destructures (`Some(Point { x, y })`) flow
        through the recursive lowering path directly. Deleted
        `try_elaborate_tuple_match`, `gen_tuple_match_arm`,
        `wrap_match_arm_body_with_destructures`,
        `is_tuple_match_arm_unconditional`, `fresh_match_scr_name`,
        `emit_panic_call`, and the `recursive_pattern_lowering` flag on
        both `AstGen` and `Sema`. `try_elaborate_refutable_nested_match`
        remains for `Some(Some(x))`-style refutable-nested variant arms
        because CFG cascading dispatch does not yet project through
        enum payloads for nested discriminant tests.
  - Remove the `--recursive-pattern-lowering` flag and make the
    recursive path the default.
  - Delete `try_elaborate_irrefutable_match`,
    `try_elaborate_tuple_match`, and
    `try_elaborate_refutable_nested_match` from
    `gruel-rir/src/astgen.rs`.
  - Delete `gen_match_arm_pattern`'s `__nested_pat_N` synthesis
    path; the `nested` threading parameter goes away.
  - Delete `wrap_match_arm_body_with_destructures`.
  - Delete the five `panic!("... ADR-0049 Phase 5 ...")` sites.
  - Delete the `@panic("non-exhaustive match")` injection in
    astgen; CFG's fallthrough owns it.
  - Remove the `fresh_nested_pat_name` counter if no longer used
    elsewhere.
  - `make test` green.

- [x] **Phase 5: Recursive usefulness / exhaustiveness**
  - Replace the current exhaustiveness check with the Maranget
    usefulness algorithm over recursive `AirPattern`s.
  - Collect witnesses as a `Vec<AirPattern>` and format them
    via the Display impl from Phase 1.
  - Populate `ErrorKind::NonExhaustiveMatch { missing }` with
    nested pattern strings.
  - Useless-arm detection (arms unreachable because earlier arms
    already cover them) is a natural byproduct of the algorithm;
    surface it as a warning (`unreachable match arm`) if it's
    cheap, otherwise defer.
  - Spec + UI tests: nested witnesses for
    `Option<Option<i32>>`, `Result<(i32, i32), i32>`, struct
    variants, integer scrutinees (still `_`).

- [x] **Phase 6: Close ADR-0049's remaining checklist items**
  - Marked ADR-0049's "Future work still on the ADR checklist"
    bullet `[x]` with a cross-reference to this ADR.
  - Previously-panicking shapes (top-level `Struct` / `Tuple` /
    `Ident` in multi-arm matches, tuple `..` rest in every position,
    irrefutable nested sub-patterns in variant fields, merged
    refutable-nested arms) are exercised by existing unconditional
    spec tests — no preview gates remain on any pattern-matching
    test.
  - Remove ADR-0049's "Future work still on the ADR checklist"
    bullet (nested-pattern witnesses) by pointing to this ADR.
  - Update ADR-0049's status line to reference ADR-0051 for
    the remaining shape completions. (Per project convention,
    old ADRs aren't rewritten — only status / cross-references.)
  - Add regression tests for every shape that used to panic,
    now as normal spec tests (not preview-gated).

- [x] **Phase 7: Cleanup, benchmarks, stabilisation**
  - ADR frontmatter: `status: implemented`, `accepted: 2026-04-23`,
    `implemented: 2026-04-23`.
  - Astgen dead-helper audit: the tuple-match elaborator and
    `__nested_pat_N` machinery were removed in Phase 4c part 4; the
    workspace builds warning-clean. The refutable-nested elaborator
    intentionally stays until CFG cascading dispatch learns to
    project through enum payloads (`Some(Some(v))` shapes) — tracked
    as open question in the refutable-nested section above.
  - Runtime-codegen / compile-time benchmarks: deferred; the full
    test suite (1706 spec + 75 UI + all unit tests) is green on the
    recursive lowering path, which is the main correctness gate.
  - Profile match-heavy programs (representative gruel-spec
    corpus + one Option-chain microbenchmark) for compile-time
    regression; the recursive lowering has more CFG blocks but
    fewer astgen passes. Target: within 10% of pre-ADR-0051
    compile time, ideally faster.
  - Runtime codegen: inspect LLVM IR for a handful of nested
    matches; verify the resulting switches match or beat the
    if-chain shape in branch count.
  - Audit `gruel-rir/src/astgen.rs` for dead helpers
    (is_leaf_sub_pattern, synthetic-name machinery) and remove.
  - ADR frontmatter: `status: implemented`, dates filled.

## Consequences

### Positive

- **Closes all remaining ADR-0049 gaps**: every panicking shape
  compiles, and nested exhaustiveness diagnostics render with
  user-visible witnesses.
- **One extension point for future pattern features**. `if let`,
  or-patterns, range patterns, and guards extend `AirPattern` + the
  recursive lowerer; they don't add new elaboration passes.
- **Smaller astgen**. Three elaborators and their helpers go away.
  `gruel-rir/src/astgen.rs` shrinks by an estimated 500–700 lines.
- **Standard algorithm for exhaustiveness**. Maranget usefulness is
  well-studied, has reference implementations in Rust, OCaml, and
  Haskell, and behaves uniformly across all pattern kinds.
- **No user-visible semantic changes**. Existing programs keep
  compiling; the only outward difference is that formerly-panicking
  shapes now work and witnesses render as nested patterns.

### Negative

- **Bigger one-shot change**. Phases 1–4 span sema, CFG, and astgen
  simultaneously. The internal flag + alongside-old-path strategy
  in Phases 2–3 contains the risk, but the Phase 4 cutover is an
  atomic change across several crates.
- **New AIR encoding complexity**. Recursive `AirPattern` needs a
  careful extra-array layout; encoding bugs can silently corrupt
  match lowering. Mitigated by Phase 1's round-trip unit tests
  being exhaustive.
- **Potentially larger CFG**. A deeply nested pattern produces more
  blocks than today's if-chain elaboration. LLVM should fold
  adjacent constant switches, but pathological cases may hit more
  IR before optimisation.
- **Maranget usefulness has a reputation for subtle bugs**. Rust's
  implementation has had a multi-year stream of edge-case fixes.
  Mitigated by initially only handling the pattern kinds
  ADR-0049 supports (no or-patterns, no ranges, no guards), which
  is a strict subset of the hard cases.

### Neutral

- **Let-destructure stays on `StructDestructure`**. Its shape is
  a tree of projections + binds with no dispatch; rewriting it
  in `AirPattern` terms would add encoding overhead without
  benefit.
- **`name @ pattern`** is available in the AIR shape but not
  exposed syntactically. This is forward compatibility, not a
  user-visible feature.

## Open Questions

1. **When to fold adjacent literal arms into `Terminator::Switch`?**
   A peephole in CFG construction or in a later opt pass? Proposed:
   opt pass, since the construction-time version duplicates the
   existing switch-building logic. Not in scope for Phase 3.

2. **Should `Bind` retain `inner: Option<...>` today even though
   `name @ pat` is unreachable syntax?** Proposed: yes. It costs
   one `Option` per bind node in the encoded form and makes the
   future `name @ pat` ADR a single parser change.

3. **Compile-time panic vs. runtime panic for non-exhaustive
   matches proved non-exhaustive at sema time.** Today, non-
   exhaustive matches are *compile errors* (`ErrorKind::
   NonExhaustiveMatch`). The runtime `@panic` fallback from
   Phase 5a exists only for shapes that astgen couldn't prove
   exhaustive but sema's exhaustiveness check hadn't run on yet.
   With Phase 5's recursive usefulness algorithm, every non-
   exhaustive match is caught at compile time — the runtime
   `@panic` becomes unreachable dead code. Proposed: emit
   `Terminator::Unreachable` and let LLVM prune; drop the
   runtime `@panic` call entirely. Revisit if we later add
   guards (which can make exhaustiveness conditional).

4. **Should we also fold let-destructure into the recursive
   pattern pipeline?** Proposed: no. The two are functionally
   separate (let: always irrefutable tree of projections; match:
   dispatch + projections), and unifying adds encoding overhead
   for no simplification. Revisit if `if let` motivates sharing
   the code paths.

## Future Work

- **`if let` / `while let`** — the direct consumer of this
  infrastructure. The recursive pattern + CFG lowering
  already has everything needed; `if let pat = scr { ... }
  else { ... }` lowers to a single-arm match with an explicit
  else branch.
- **Or-patterns (`A | B`)** — adds `AirPattern::Or { alts:
  Vec<AirPattern> }`, recursively lowered as a disjunctive
  switch plus binding-set consistency check.
- **Range patterns (`1..=5`)** — adds `AirPattern::IntRange`,
  lowered as a pair of `icmp` + `and`.
- **Guards (`arm if cond`)** — CFG lowering adds a guard-check
  block after the pattern match but before the arm body.
- **`name @ pattern`** — surface-syntax exposure of the
  already-present `Bind.inner` slot.
- **Useless-arm warnings** — if the usefulness algorithm's
  byproduct output is wired through, these fall out for free.

## References

- [ADR-0036: Struct Destructuring and Partial Move Ban](0036-destructuring-and-partial-move-ban.md)
- [ADR-0037: Enum Data Variants and Full Pattern Matching](0037-enum-data-variants-and-full-pattern-matching.md)
- [ADR-0048: First-Class Tuples](0048-tuples.md)
- [ADR-0049: Nested Destructuring and Pattern Matching](0049-nested-destructuring-and-patterns.md) — this ADR completes its §8 "Alternative considered" path and closes its remaining checklist item.
- [Warnings for pattern matching — Luc Maranget, JFP 2007](http://moscova.inria.fr/~maranget/papers/warn/warn.pdf) — usefulness/exhaustiveness algorithm for Phase 5.
- [Rust Reference: Patterns](https://doc.rust-lang.org/reference/patterns.html)

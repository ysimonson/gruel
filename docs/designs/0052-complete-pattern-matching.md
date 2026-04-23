---
id: 0052
title: Complete Recursive Pattern Matching
status: proposal
tags: [patterns, pattern-matching, cfg, exhaustiveness, maranget]
feature-flag:
created: 2026-04-23
accepted:
implemented:
spec-sections: ["4.7"]
superseded-by:
---

# ADR-0052: Complete Recursive Pattern Matching

## Status

Proposal

## Summary

Finish what ADR-0051 started: lower `Some(Some(v))`-style refutable
nested variant arms through the recursive CFG cascading dispatch
instead of the `try_elaborate_refutable_nested_match` AST rewrite, then
delete that last elaborator. At the same time, harden the Phase 5
Maranget usefulness algorithm against the edge cases that have bitten
Rust's implementation (deeply-nested constructors, redundant
catch-alls, overlapping literal arms, subsumed variant arms) with a
dedicated corner-case test suite. After this, every pattern shape
flows through one code path (AIR recursive dispatch), and
exhaustiveness coverage for the shapes Gruel supports has the same
structural guarantees Rust's checker has earned over the years.

## Context

### What ADR-0051 left behind

ADR-0051 Phase 4c retired `try_elaborate_irrefutable_match`'s
multi-arm tuple-root sibling (`try_elaborate_tuple_match`) and moved
irrefutable nested destructures onto the new
`RirPatternBinding.sub_pattern` slot. One elaborator still runs on the
default path:

- `try_elaborate_refutable_nested_match`: rewrites arms whose variant
  fields carry a refutable sub-pattern (e.g.,
  `Some(Some(v)) => ...`) into an outer match whose arms wrap an
  *inner* match on the projected payload, falling back to the outer
  wildcard arm on mismatch.

The elaborator stays because the CFG cascading dispatch introduced in
ADR-0051 Phase 3 / 4b part 2 only projects through *struct and tuple*
fields. Enum payload access needs the `EnumPayloadGet` AIR / CFG
instruction, which `emit_pattern_test` does not yet emit. Without
that, a pattern like `EnumDataVariant(Some, [EnumDataVariant(Some,
[Bind v])])` cannot be dispatched by the recursive lowering — CFG
checks the outer discriminant and stops.

Keeping the elaborator is correct for today's shapes but costs:

1. **Two pipelines.** Astgen now has two ways to reach sema:
   recursive RIR for most shapes, rewritten AST for this one. Every
   future refutable-nested feature (or-patterns, range patterns,
   guards) has to work through both or consciously disable one.
2. **Witnesses are structural, not rewritten.** ADR-0051 Phase 5's
   Maranget algorithm runs on the AIR tree, so when
   `try_elaborate_refutable_nested_match` fires the exhaustiveness
   diagnostic sees the synthesised `match __refut_0 { ... }` shape
   rather than the user's original `Some(Some(v))`. Witness rendering
   paints the inner `__refut_0` name into error messages for these
   cases.
3. **Subtle control-flow differences.** The elaborator introduces
   intermediate scrutinees and re-checks the outer arm's wildcard
   fallback. Reasoning about drop ordering, `StorageLive` timing, and
   match-lowered code size becomes a case analysis across two
   compilation paths.

### What Rust's usefulness checker taught the world

Rust's pattern usefulness algorithm has been the subject of
long-running follow-ups. Within the shapes Gruel actually supports
today, the issues worth auditing against are:

- **Deeply-nested constructors**: `Some(Some(Some(1)))` triples the
  recursion. The algorithm must terminate, produce a finite witness
  list, and not blow the stack.
- **Redundant catch-all**: `match b { true => 1, false => 2, _ => 3 }`
  — the `_` arm is unreachable. Our sema tracks per-arm reachability
  via `ctx.warnings.push(UnreachablePattern ...)` but only for
  duplicate literals. The usefulness algorithm's byproduct
  (`arm_reachability` in `usefulness.rs`) is presently unwired.
- **Shadowing across constructors**: `match v { Some(_) => 1, Some(5)
  => 2, None => 3 }` — the second arm is unreachable because the
  first already covers every `Some`. Surface as `unreachable match
  arm` warning.
- **Overlapping wildcards with subsumption**: `match x { (1, _) => 1,
  (_, 2) => 2, (1, 2) => 3, _ => 4 }` — the third arm is unreachable.
  Maranget computes this correctly if `specialize_rows` is right.
- **Nested variant exhaustiveness without a wildcard**:
  `match x { Some(Some(v)) => ..., Some(None) => ..., None => ... }`
  must count as exhaustive only once refutable-nested arms flow
  through the recursive path (Phases 1-3 below).

In scope, addressed in this ADR:

- **Zero-arm matches on uninhabited scrutinees** — `!` exists
  (ADR-0001) and zero-variant enums lower to `Type::NEVER`, but
  sema's `analyze_match` rejects every zero-arm match with
  `ErrorKind::EmptyMatch` regardless of scrutinee type. Relaxing
  that gate for uninhabited scrutinees closes the Maranget
  empty-signature path (a vacuously exhaustive match) and is a
  natural completeness improvement while we're in this code.

Out of scope because the underlying language feature isn't there
today:

- **Integer ranges** — `type_signature` returns `None` for integers,
  so the algorithm always reports `_` as the sole witness; a
  dedicated range-patterns ADR can lift this later.
- **Or-patterns (`A | B`)** — no surface syntax; AIR has no `Or`.
- **Guards (`arm if cond`)** — reachability becomes conditional,
  which usefulness alone can't decide.
- **`name @ pat`** — reserved in `AirPattern::Bind.inner` but not
  surfaced in syntax, so there's no witness rendering target.

## Decision

Do two things:

1. **Project through enum payloads in the CFG cascading dispatch** so
   `emit_pattern_test` can descend into nested `EnumDataVariant` /
   `EnumStructVariant` field patterns directly. Retire
   `try_elaborate_refutable_nested_match` and the `__nested_pat_N` /
   `__refut_N` synthetic-name machinery it depends on.
2. **Harden the Maranget usefulness tests** against the edge cases
   above: empty enums, deep nesting, overlapping / redundant arms,
   nested witness rendering, and (as a bonus while we're in there)
   wire `usefulness::arm_reachability` into the sema diagnostics so
   redundant catch-alls warn uniformly.

### 1. Enum payload projection in CFG

Today's `emit_pattern_test` walks `Tuple` and `Struct` patterns by
extending a `Projection` list and re-reading the spilled scrutinee
via `PlaceRead`. The list-of-projections path is good — projections
compose cleanly — but it has no variant for enum payloads. The
minimally-disruptive fix is to do the projection inline rather than
extend the `Projection` enum: when we enter an `EnumDataVariant` /
`EnumStructVariant` pattern with non-trivial field patterns, after
the discriminant check succeeds, emit `CfgInstData::EnumPayloadGet`
for each field and recurse into that field's pattern using the
projected value as the new scrutinee.

Concretely, the `emit_pattern_test` signature gains an alternate
entry point that takes a `CfgValue` scrutinee instead of a
`(scr_slot, Vec<Projection>)` pair; the recursive call path flips
between them as needed. Struct / tuple projections continue to go
through the `Vec<Projection>` path (so the spill is shared); enum
field projections go through `EnumPayloadGet` and then back into the
`Vec<Projection>` path with a fresh slot spill of the payload value.

After the variant check:

```text
enum_slot := spill(scrutinee)
discriminant := PlaceRead(enum_slot)
if discriminant == variant_index {
    for each field i in the variant:
        field_val := EnumPayloadGet(PlaceRead(enum_slot), variant_index, i)
        field_slot := spill(field_val)
        recurse emit_pattern_test(field_slot, [], field_pattern,
                                   /* matched */ next_field_or_arm,
                                   /* unmatched */ arm_fallthrough)
} else {
    goto arm_fallthrough
}
```

Field spills reuse the existing temp-local mechanism
(`alloc_temp_local`). The per-field spill is cheap; LLVM's mem2sse
pass collapses the alloca / store / load chain for non-escaping
temporaries, which is exactly this shape.

### 2. Stop inline-extracting bindings for refutable nested arms

Sema's existing `emit_recursive_pattern_bindings` unconditionally
emits `StorageLive` + `Alloc` for every Ident leaf inside a
`sub_pattern`, which is fine when the whole arm is guaranteed to
succeed (irrefutable nested case). For refutable sub-patterns this
over-extracts: bindings inside a Some-field are only live when the
inner Some matched, but sema pre-extracts them before CFG's dispatch
has run.

Today this is masked because the elaborator rewrites refutable
nested arms into a nested match, so sema only ever sees irrefutable
sub-patterns in the field-extraction path. Removing the elaborator
exposes the mismatch. The fix: CFG's cascading dispatch owns
introducing bindings for Ident leaves it encounters inside enum
fields. Sema's walker becomes Tuple / Struct-only; DataVariant and
StructVariant sub-pattern traversal moves into CFG.

### 3. Useless-arm detection via Maranget

`usefulness::arm_reachability` is already implemented and idle.
Wire it into `analyze_match` after the exhaustiveness check: for each
arm, if `is_useful(P_<i, [arm_i])` returns `NotUseful`, push
`WarningKind::UnreachablePattern` with the arm's pattern rendered via
`render_witness`. The existing per-literal / per-variant duplicate
detection becomes a special case; remove the ad-hoc `seen_ints`,
`bool_true_span`, and `covered_variants` bookkeeping once parity is
confirmed. Watch for double-warning: each arm should fire at most
one `UnreachablePattern` diagnostic.

### 4. Delete the refutable elaborator

With (1)-(3) in place, delete:

- `AstGen::try_elaborate_refutable_nested_match` and its three
  helpers (`merge_group_single_field`, `merge_group_data_variant`,
  `merge_group_struct_variant`, `replace_refutable_nested_subs`,
  `fresh_refutable_elab_name`).
- The `__nested_pat_N` counter (`nested_pat_counter` +
  `fresh_nested_pat_name`) — `sub_pattern` already replaced its
  producers in ADR-0051 Phase 4c part 4; the helpers only survive
  because `try_elaborate_refutable_nested_match` still calls them.
- The `is_irrefutable_destructure` dead-ish helper (used only by
  `try_elaborate_irrefutable_match`, which stays; check whether the
  call site is still reachable and trim accordingly).

All pattern lowering funnels through `gen_match_arm_pattern` → sema
recursive `lower_pattern` → CFG recursive `emit_pattern_test`. One
path, end to end.

### 5. Zero-arm match on uninhabited scrutinees

`analyze_match` currently rejects zero-arm matches with
`ErrorKind::EmptyMatch` before any type-directed logic runs. Relax
the check so a zero-arm match whose scrutinee has an uninhabited
type (`Type::NEVER`, or a zero-variant enum — both surface as
`Type::NEVER` per `gruel-air/src/types.rs`) is legal and lowers to a
CFG block with `Terminator::Unreachable`. Any zero-arm match on an
inhabited type remains an error.

This is the only change required to close the "match exhaustiveness
on uninhabited signatures" corner of the usefulness algorithm:
the Maranget check already returns `NotUseful` (i.e., exhaustive)
when the head type's signature is `Some(vec![])` and no arms are
present, but today's pipeline never reaches it. With sema's gate
relaxed, the corner fires naturally.

Codegen: `Terminator::Unreachable` → LLVM `unreachable` instruction.
No runtime panic: the match is statically impossible to reach, so
LLVM can optimise the branch out entirely in release builds.

### 6. Maranget corner-case test suite

Add regression cases to `crates/gruel-spec/cases/expressions/match.toml`
covering:

- `zero_arm_match_on_never_is_exhaustive`: a function with an `n: !`
  parameter whose body is `match n { }` compiles.
- `zero_arm_match_on_empty_enum_is_exhaustive`: same, with an empty
  `enum V {}` parameter (Type::NEVER via the zero-variant path).
- `zero_arm_match_on_inhabited_still_errors`: `match 0 { }` remains
  an `EmptyMatch` error.
- `deeply_nested_missing_witness`: a hand-monomorphised
  `Option<Option<Option<...>>>` stack (Gruel has no generics; declare
  each nesting level as its own enum). Verify the witness names the
  specific uncovered shape when a deep `Some(Some(Some(1)))` arm is
  the only non-wildcard.
- `redundant_catchall_warns`: `match b { true => 1, false => 2, _ =>
  3 }` — compiles; emits `unreachable match arm` on the `_` arm.
- `shadowed_variant_arm_warns`: `match o { Opt::Some(_) => 1,
  Opt::Some(5) => 2, Opt::None => 3 }` — second arm emits
  unreachable, consistent with the other redundancy diagnostics.
- `tuple_overlap_warns`: `match t { (1, _) => 1, (_, 2) => 2, (1, 2)
  => 3, _ => 4 }` — third arm emits unreachable.
- `nested_variant_both_branches_exhaustive`:
  `match x { Opt::Some(Opt::Some(v)) => v, Opt::Some(Opt::None) =>
  0, Opt::None => -1 }` — exhaustive without a catch-all. Requires
  the Phase 1-3 work that removes the refutable elaborator.
- `nested_variant_missing_inner_some`: drop the `Some(Some(v))` arm;
  error message names the specific uncovered nested shape.
- `struct_variant_missing_field_combo`:
  `enum E { V { a: bool, b: bool } } match e { E::V { a: true, b:
  true } => 1 }` — reports the three missing field combinations.
- `integer_scrutinee_only_wildcard_exhausts`: `match n { 0 => 1 }`
  — error names `_` (integer signatures are open).

Each test is small, self-contained, and unconditional (no preview
gates). They protect against regressions in the usefulness algorithm
within the shapes Gruel supports today. Edge cases that depend on
features we don't have yet (empty types, `name @ pat`, or-patterns,
guards, ranges) are tracked in the §Future-Work section of this ADR
and picked up by future ADRs.

### 7. Useless-arm diagnostic wording

Our existing `UnreachablePattern` warning takes a `String` describing
the pattern and optionally a label pointing at the first-covering
pattern's span. The Maranget wiring should continue to populate both
fields: render the unreachable arm's pattern for the primary message
and the first earlier arm that subsumes it for the label. Finding the
"first earlier arm" falls out of the algorithm: run `is_useful(P_{<i},
[arm_i])`; if `NotUseful`, scan forward from 0 until adding `arm_j`
makes `arm_i` unuseful — that's the offender. The scan is O(arm_count)
which fits in the diagnostic path.

## Implementation Phases

- [ ] **Phase 1: CFG enum payload projection**
  - Extend `emit_pattern_test` with a `CfgValue`-scrutinee entry
    point for enum field recursion.
  - Handle `EnumDataVariant` / `EnumStructVariant` patterns with
    non-trivial field patterns: after the discriminant branch, emit
    `EnumPayloadGet` per field, spill to a temp local, recurse.
  - Unit tests: cascading dispatch builds CFG successfully for
    `Some(Some(v))`, `Ok(Err(Point { x, y }))`, etc., with the
    refutable elaborator forced off via a temporary override (revert
    at end of phase).

- [ ] **Phase 2: Move binding introduction for refutable nested arms
    into CFG**
  - CFG's cascading dispatch allocates local slots for Ident leaves
    inside variant fields (mirroring sema's existing
    `emit_recursive_pattern_bindings` path).
  - Sema stops pre-extracting bindings for refutable sub-patterns.
    Irrefutable Tuple / Struct / Ident sub-patterns keep the
    existing sema-driven extraction (no reason to move it).
  - Regression tests from ADR-0051's `nested_match_*` and
    `refutable_nested_*` suites continue to pass.

- [ ] **Phase 3: Delete `try_elaborate_refutable_nested_match`**
  - Remove the elaborator and its helpers
    (`merge_group_single_field`, `merge_group_data_variant`,
    `merge_group_struct_variant`, `replace_refutable_nested_subs`,
    `fresh_refutable_elab_name`, `fresh_nested_pat_name`,
    `nested_pat_counter`).
  - `debug_assert!(nested.is_empty())` on the match-arm loop is
    replaced with `let _ = nested` or the parameter goes away.
  - Update ADR-0051's open question (3) and frontmatter note about
    the remaining elaborator.

- [ ] **Phase 4: Useless-arm detection via Maranget**
  - Wire `usefulness::arm_reachability` into `analyze_match` and
    emit `WarningKind::UnreachablePattern` per unreachable arm.
  - Identify the first-covering arm for the label via the scan
    described in §Decision-7.
  - Retire the ad-hoc per-literal / per-variant duplicate detection
    once parity is confirmed on the UI-test suite.

- [ ] **Phase 5: Zero-arm match on uninhabited scrutinees**
  - Relax `analyze_match`'s `EmptyMatch` gate for uninhabited
    scrutinees (per §Decision-5). Emit `Terminator::Unreachable` for
    the CFG body.
  - Inhabited zero-arm matches remain an error.

- [ ] **Phase 6: Maranget corner-case test suite**
  - Add the eleven cases listed in §Decision-6 to
    `crates/gruel-spec/cases/expressions/match.toml`. Each is either
    `compile_fail = true` with a specific `error_contains`, a
    reachable program with `exit_code = N`, or a compiling program
    that should emit a `warning_contains` entry for the unreachable
    arm.
  - Verify via `make test`.

- [ ] **Phase 7: ADR stabilisation**
  - Update ADR-0051's note about the remaining elaborator (the
    open question closes).
  - ADR-0052 frontmatter: `status: implemented`, dates filled in.
  - `make test` green.

## Consequences

### Positive

- **Single pipeline.** All pattern shapes lower through the recursive
  AIR / CFG dispatch. New pattern features (`if let`, or-patterns,
  guards, ranges) extend `AirPattern` + `emit_pattern_test`; no
  parallel astgen rewrite path to maintain.
- **Correct nested witnesses for every shape.** The exhaustiveness
  checker always sees the user's original pattern tree, so missing
  patterns for `Some(Some(v))`-style arms render as nested shapes
  instead of referring to synthesised `__refut_N` scrutinees.
- **Useless-arm detection unified.** One algorithm catches redundant
  literals, duplicate variants, overlapping tuples, and subsumed
  nested arms. Replaces two ad-hoc trackers.
- **Astgen shrinks further.** Estimated 300-400 additional lines
  removed from `gruel-rir/src/astgen.rs` on top of Phase 4c's 426.
- **Corner-case coverage.** The Maranget suite locks in behaviour
  that Rust's checker took years to get right.

### Negative

- **CFG complexity grows.** `emit_pattern_test` gains a second
  scrutinee form and an explicit enum-payload spill path. More
  surface area for subtle control-flow bugs.
- **Temp-local churn.** Each enum field projection spills to a
  fresh slot. LLVM will optimise most of it away, but debug-build
  IR grows.
- **Binding-ownership split.** Irrefutable sub-patterns keep their
  sema-emitted extraction while refutable ones move to CFG. The
  split is principled (irrefutable = always live, refutable =
  conditionally live) but adds a decision point contributors need
  to read.
- **Maranget subtleties.** Witness reconstruction across
  specialisation / default-matrix steps has had historical bugs;
  the corner-case suite mitigates but doesn't eliminate risk.

### Neutral

- **No user-visible semantics change** (except that previously
  "works" remains "works" and error messages improve for nested
  shapes).
- **`try_elaborate_irrefutable_match` stays.** Single-arm irrefutable
  match → let-destructure has semantics (drop ordering, field-level
  lifetime scoping) that the recursive path doesn't mirror exactly.
  Revisit independently once `if let` lands.

## Open Questions

1. **Field-projection spill strategy.** Each enum-field recursion
   currently spills to a fresh temp local. An alternative is to
   thread a `Vec<Projection>` with an `EnumPayload` variant and
   re-project from the original scrutinee slot on demand. The spill
   form is simpler; the projection-list form is fewer locals.
   Proposed: start with spills and measure. If debug-build size
   regresses on the spec corpus by > 5%, revisit.

2. **Where do useless-arm warnings point?** Rust points at the
   unreachable arm and labels the first earlier arm that covered
   it. Gruel already has a `with_label` diagnostic builder. The
   first-covering-arm scan is O(n); fine at typical arm counts.
   Larger matches (auto-generated switches) might prefer a
   non-precise "an earlier arm covers this value". Proposed: do
   the precise scan always; add a threshold later if needed.

3. **Should we bail out of reachability analysis once the match has
   an error?** A non-exhaustive match can trivially have unreachable
   arms because the analyzer's matrix is incomplete. Currently
   sema reports non-exhaustive and returns — no reachability runs.
   Keep that behaviour: reachability fires only on exhaustive
   matches. No change needed; documented here so future readers
   don't wonder.

## Future Work

- **Or-patterns (`A | B`)**. Requires `AirPattern::Or` + usefulness
  specialisation expansion.
- **Range patterns (`1..=5`, `'a'..='z'`)**. Adds `AirPattern::Range`
  + integer-range signature for exhaustiveness.
- **Guards (`arm if cond`)**. Reachability becomes conditional;
  guarded arms are always considered useful by usefulness (they
  might not match), but exhaustiveness has to treat them as
  potentially non-covering.
- **Empty-type elimination**. If Gruel gains a true `!` type, an
  empty scrutinee arm needs `Terminator::Unreachable` without
  sema raising a non-exhaustive error.

## References

- [ADR-0049: Nested Destructuring and Pattern Matching](0049-nested-destructuring-and-patterns.md)
- [ADR-0051: Recursive Pattern Lowering](0051-recursive-pattern-lowering.md) —
  the direct predecessor. This ADR closes Phase 5 §Open-Questions-3
  and the refutable-nested elaborator gap.
- [Warnings for pattern matching — Luc Maranget, JFP 2007](http://moscova.inria.fr/~maranget/papers/warn/warn.pdf)
- [Rust `rustc_pattern_analysis` crate](https://github.com/rust-lang/rust/tree/master/compiler/rustc_pattern_analysis) —
  historical reference for corner cases worth covering.
- [Rust RFC 1872: Exhaustive Integer Patterns](https://rust-lang.github.io/rfcs/1872-exhaustive-integer-matching.html) —
  out of scope here; informs the future-work range-patterns ADR.

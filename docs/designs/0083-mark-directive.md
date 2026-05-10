---
id: 0083
title: `@mark(...)` directive for marker traits
status: proposal
tags: [types, ownership, syntax, directives]
feature-flag: mark_directive
created: 2026-05-10
accepted:
implemented:
spec-sections: ["2.5", "3.8"]
superseded-by:
---

# ADR-0083: `@mark(...)` directive for marker traits

## Status

Proposal

## Summary

Replace the `copy` / `linear` declaration-site keywords introduced in
ADR-0080 with a `@mark(...)` directive: `@mark(copy) struct Foo { … }`,
`@mark(linear) enum Bar { … }`. Markers live in a small open registry in
`gruel-builtins` (initially `copy` and `linear`) so future
declaration-time-only markers — Gruel's equivalent of marker traits —
plug in by adding one row. Semantics, consistency rules, and codegen are
unchanged from ADR-0080; only the surface syntax moves from a contextual
keyword slot into the directive system, where it sits next to `@derive`
and `@lang` instead of in front of `struct` / `enum`.

## Context

ADR-0080 carved out a posture slot in struct/enum heads to carry `copy`
and `linear`. That slot solves the immediate problem (Copy and Linear
are *postures*, not method-bearing interfaces) but introduces a third
syntactic mechanism for type-level metadata:

| Mechanism | Where | Carries |
|-----------|-------|---------|
| Keyword (ADR-0080) | head, before `struct`/`enum` | posture (`copy`, `linear`) |
| `@derive(…)` (ADR-0058) | directive list | method-bearing interface implementations |
| `@lang(…)`, `@handle` | directive list | compiler-recognized roles |

The keyword slot pays for itself only as long as posture is the *only*
marker-style attribute we ever want. As soon as a second marker appears
(an obvious near-term candidate is a marker indicating that an enum is
intended for use as a discriminated capability tag, but other examples
exist), the keyword pattern doesn't extend — a third reserved word in
the head, a fourth, etc., quickly becomes worse than a directive list.
Even the existing two keywords already hit awkward edges:

- `copy` had to be implemented as a *contextual* identifier (not a hard
  keyword) so the prelude's `fn copy(self) -> Self` and user methods
  named `copy` keep working. The parser carries a special-case
  `posture_parser` that filters `Ident("copy")` next to a hard
  `Linear` token.
- The two markers occupy the same exclusive slot via grammar alone —
  any future markers that want to coexist with `copy` or `linear`
  cannot use the slot.
- Anonymous `struct` / `enum` literals duplicate the keyword logic at
  every literal site.

Moving these into the directive system unifies how *all*
declaration-time markers are spelled, removes the contextual-keyword
hack, frees the `Linear` reserved word, and gives a single extension
point — `BUILTIN_MARKERS` in `gruel-builtins` — for future markers.

## Decision

### Syntax

```gruel
struct Point { x: i32, y: i32 }        // inferred Copy (all members Copy)
struct Mixed { x: i32, y: String }     // inferred Affine
struct Held { fd: FileHandle }         // inferred Linear (FileHandle is linear)

@mark(copy)   struct Point { x: i32, y: i32 }   // assertion: errors if not Copy
@mark(affine) struct Token { x: i32 }           // suppresses Copy inference
@mark(linear) struct Pin { x: i32 }             // override: Linear despite Copy members

@mark(copy)   struct { x: i32 }        // anonymous Copy struct (assertion)
@mark(linear) enum   { A, B }          // anonymous Linear enum

@derive(Clone) @mark(copy) struct Pair { x: i32, y: i32 }
@mark(copy, future_marker) struct …    // multiple markers in one directive
```

`@mark` lives in the same directive list as `@derive`, `@lang`, and
`@allow`. Order between directives is irrelevant. A type can carry
multiple `@mark(…)` directives; the marker set is the union. Mutual
exclusion (Copy ⊥ Linear) is enforced regardless of whether the two
markers appear in one directive or two.

### Marker registry

A new `BUILTIN_MARKERS` table in `gruel-builtins/src/lib.rs` lists the
recognized marker names and their semantics in one place:

```rust
pub struct BuiltinMarker {
    pub name: &'static str,
    pub kind: MarkerKind,           // Posture(Copy) | Posture(Linear) | …
    pub applicable_to: ItemKinds,   // struct | enum | both
}

pub static BUILTIN_MARKERS: &[BuiltinMarker] = &[
    BuiltinMarker { name: "copy",   kind: MarkerKind::Posture(Posture::Copy),   applicable_to: ItemKinds::STRUCT_OR_ENUM },
    BuiltinMarker { name: "affine", kind: MarkerKind::Posture(Posture::Affine), applicable_to: ItemKinds::STRUCT_OR_ENUM },
    BuiltinMarker { name: "linear", kind: MarkerKind::Posture(Posture::Linear), applicable_to: ItemKinds::STRUCT_OR_ENUM },
];
```

The registry serves three purposes:

1. **Closed taxonomy** — sema rejects `@mark(unknown)` with a
   suggest-from-registry diagnostic (parallel to the directive
   diagnosis path that already exists for `@derive`).
2. **Single source of truth** — `is_copy` / `is_linear` flag-setting
   reads `MarkerKind::Posture` rather than string-matching directive
   args.
3. **Mechanical extension point** — adding a future marker is one row
   in the registry plus the sema arm that consumes its `MarkerKind`.

The registry is intentionally small. New markers must still go through
an ADR; the registry documents what *exists*, not what's permissible.

### Semantics: uniform structural inference

ADR-0080 split posture into "declared" (named types — keyword
required for Copy) versus "inferred" (tuples and the
anonymous-enum carve-out). This ADR collapses the split. Every
unmarked type — named struct, named enum, anonymous struct,
anonymous enum, tuple, array — infers posture from its members
using one rule:

- If **any** member is Linear → the type is Linear.
- Else if **every** member is Copy → the type is Copy.
- Else → the type is Affine.

The marker overlay sits on top:

- `@mark(copy) struct/enum X { … }` — *Copy assertion*. The type
  is declared Copy, and the inference rule must agree (every
  member Copy). If a member isn't Copy, the directive is
  rejected with the field cited. Useful for documenting intent
  and turning a silent posture downgrade (adding a non-Copy
  field later) into an error at the declaration site.
- `@mark(affine) struct/enum X { … }` — *Copy suppressor*. The
  type is declared Affine regardless of whether inference would
  produce Copy. Use when a type's members all happen to be Copy
  but its semantics demand move-on-use (a non-Clone-able token,
  a single-use builder, a value whose duplication would be
  semantically wrong even though it's bitwise-fine). Has no
  effect on Linear inference: if any member is Linear, the
  directive is rejected — Linear is contagious and cannot be
  hidden behind an affine declaration.
- `@mark(linear) struct/enum X { … }` — *Linear override*.
  The type is declared Linear regardless of member postures.
  Linear can hold anything (ADR-0080), so the only thing this
  precludes is structural inference picking Copy or Affine. Use
  when the type has linear semantics that aren't visible from
  its fields (e.g. an `i32` handle that's actually a kernel
  resource ID).
- No `@mark` (or `@mark` with no posture marker) → posture is
  whatever inference produces. No diagnostic, even if the
  resulting posture changes when a field changes.
- Copy ⊥ Drop is unchanged — a type with `fn drop` cannot be Copy
  (whether declared or inferred). `Vec(T)` and any other type
  with a Drop impl is therefore *never* Copy regardless of its
  members. This is the only carve-out in the model.
- Mutual exclusion: at most one of `copy` / `affine` / `linear`
  per item. Any combination (one directive with multiple
  posture args, or two directives carrying conflicting markers)
  is rejected.

The arithmetic of "if any field is linear, the type is linear"
remains *strictly enforced* even with `@mark(copy)`: declaring
Copy on a type with a Linear field is an error, not a silent
override. Linear is contagious upward and cannot be hidden.

**Consequences for built-ins under the uniform rule:**

| Type | Posture |
|------|---------|
| `(i32, i32)` | Copy (all members Copy) |
| `(i32, String)` | Affine (one affine member, no linear) |
| `(i32, FileHandle)` | Linear (one linear member) |
| `[i32; 3]` | Copy — *changes back from ADR-0080's "always affine"* |
| `[String; 3]` | Affine |
| `[FileHandle; 3]` | Linear |
| `Vec(i32)` | Affine (Vec has Drop, so never Copy; no linear members) |
| `Vec(FileHandle)` | Linear (Vec is linear iff T is linear, ADR-0067) |
| `Option(i32)` | Copy (anonymous-enum payload is Copy) |
| `Option(String)` | Affine |
| `Option(FileHandle)` | Linear |
| `Result(i32, i32)` | Copy |
| `Result(String, FileHandle)` | Linear |

`Option` / `Result` and other generic prelude wrappers track
their type arguments' posture automatically — no comptime
predicate, no double declaration, no anonymous-type carve-out.
Tuples behave as today. Arrays of Copy regain Copy posture
(reverting ADR-0080's "arrays are containers, not value types"
stance — this ADR judges that the consistency win outweighs the
container/value-type distinction, and arrays of large Copy types
already had `clone()` for explicit deep copies under that
distinction's intent).

`@ownership(T)` remains the comptime query for posture.

### Mutual exclusion

The validator collects *all* markers from *all* `@mark` directives on
an item, deduplicates by name, then validates pairwise constraints.
Copy + Linear is the only constraint today. Duplicate markers
(`@mark(copy) @mark(copy)`) are a soft error (warning under `@allow`,
hard error otherwise — TBD in Open Questions).

### Validator and inference entry points

Two passes on `StructDef` / `EnumDef`:

1. **Inference pass.** Compute structural posture from members:
   classify each member, fold into a posture using the rule above.
   This pass writes the type's *inferred* posture into a new field
   (`inferred_posture: Posture`) — separate from the declared bits
   so we can distinguish "user said Copy" from "I figured out
   Copy."
2. **Reconciliation pass.** If a posture marker is present:
   - `@mark(copy)` + inferred ≠ Copy → reject with the offending
     member cited. (Subsumes ADR-0080's `validate_posture_consistency`.)
   - `@mark(affine)` + inferred = Copy → accept; the declared
     posture wins, and the type is Affine.
   - `@mark(affine)` + inferred = Affine → accept (redundant but
     harmless).
   - `@mark(affine)` + inferred = Linear → reject; Linear members
     cannot be hidden by an affine declaration.
   - `@mark(linear)` + any inferred → accept; the declared
     posture wins, and the type is Linear.
   - No posture marker → declared posture *is* inferred posture.

After this pass `StructDef.is_copy` / `is_linear` (and the enum
counterparts) carry the *final* posture, which is what
codegen and the rest of sema see. The flags on `StructDecl` /
`EnumDecl` AST nodes survive only as the directive-derived "user
asserted" bits; the final posture is computed in sema.

### Diagnostic surface

- `@mark(unknown_marker)` → `unknown marker 'unknown_marker'` with
  suggestion from `BUILTIN_MARKERS` (Levenshtein, parallel to the
  directive diagnosis path).
- `@mark(copy) @mark(linear) struct …` → existing `LinearStructCopy`
  diagnostic, repointed to the offending directive span.
- `@mark(copy) enum … { A(FileHandle) }` → existing
  copy-with-affine-payload diagnostic, span on the `@mark(copy)`
  directive instead of on a `copy` keyword.
- `@mark(copy)` on a non-struct/non-enum item (`fn`, `const`,
  `interface`) → `marker 'copy' is not applicable to functions`.

### What retires (after stabilization)

- `copy_name: Spur` symbol on the parser state.
- `posture_parser` in `chumsky_parser.rs`.
- `Linear` token in `gruel-lexer` (both the logos and public token
  enums) and its parser uses (the head slot and the linear-interface
  parser at `chumsky_parser.rs:3521`).
- The parser-time mutual-exclusion check (collapses into the
  directive-arg-list parser).
- `is_copy` / `is_linear` field plumbing on `StructDecl` / `EnumDecl`
  AST nodes survives — sema still needs the bits — but is filled
  exclusively from `@mark(...)` after Phase 4.
- Spec text under `docs/spec/src/03-types/08-move-semantics.md` and
  `docs/spec/src/02-lexical-structure/05-builtins.md` describing the
  posture keyword slot, replaced by directive-form prose.

### What's added

- `@mark(...)` directive recognition: name → registry lookup → flag
  population in `register_type_names` (struct path) and
  `find_or_create_anon_struct` / `find_or_create_anon_enum`
  (anonymous path).
- `BUILTIN_MARKERS` table + `MarkerKind`, `Posture` enums in
  `gruel-builtins`.
- `mark_directive` preview gate (in `PreviewFeature`), fired in
  `register_type_names` when an `@mark(...)` directive is seen on a
  type declaration.
- Spec tests under `cases/items/mark-directive.toml` covering the new
  surface (parse, gating, mutual exclusion, unknown-marker
  diagnostic, applicability check).

## Implementation Phases

Each phase ships behind `--preview mark_directive`, ends green, quotes
its LOC delta in the commit message.

### Phase 1: Marker registry + directive recognition

- [ ] Add `MarkerKind`, `Posture`, `ItemKinds`, `BuiltinMarker`, and
      `BUILTIN_MARKERS` to `gruel-builtins/src/lib.rs`.
- [ ] Add `PreviewFeature::MarkDirective` (`mark_directive`) to
      `gruel-util/src/error.rs` (enum + `name()` + `adr()`).
- [ ] Recognize `mark` in `validate_directive_names`
      (`crates/gruel-air/src/sema/declarations.rs:281`) so `@mark` no
      longer falls through to `UnknownDirective`.
- [ ] In `register_type_names`, when a type-decl directive is `@mark`,
      gate behind `mark_directive`, look each argument up in
      `BUILTIN_MARKERS`, and dispatch:
      - Unknown name → `UnknownMarker { name, suggestions }` with
        Levenshtein suggestions from the registry.
      - `MarkerKind::Posture(Copy)` → set `is_copy = true`.
      - `MarkerKind::Posture(Linear)` → set `is_linear = true`.
      - Applicability mismatch (e.g. marker that's struct-only on an
        enum) → `MarkerNotApplicable { name, item_kind }`.
- [ ] Mutual exclusion (Copy + Linear): rejected at sema with the
      existing `LinearStructCopy` diagnostic, repointed to the
      `@mark` directive span.
- [ ] Implement uniform structural inference. New helper
      `infer_posture(members)` returns `Posture::{Copy, Affine,
      Linear}` from the type's members under the rule above.
      Used by `register_type_names` for named struct/enum,
      `find_or_create_anon_struct`, `find_or_create_anon_enum`,
      and the tuple/array posture queries in `is_type_copy` /
      `is_type_linear` (ADR-0080's per-kind branches collapse
      into one helper).
- [ ] Reconciliation pass: when `@mark(copy)` is present on a
      type, the inferred posture must be Copy or the directive
      is rejected with the offending member cited. `@mark(linear)`
      forces Linear regardless of inference. No `@mark` →
      declared posture is the inferred posture.
- [ ] Anonymous struct/enum literals run the same inference and
      reconciliation paths as named declarations. The ADR-0080
      anonymous-enum carve-out retires (subsumed by the uniform
      rule).
- [ ] `is_type_copy` for `[T; N]` returns `is_type_copy(T)`
      (revives Copy posture for arrays of Copy elements,
      consciously reverting ADR-0080 — see Open Question 2).
      `is_type_copy` for `Vec(T)` continues to return `false`
      (Vec has Drop, so Copy ⊥ Drop forbids it).
- [ ] Spec tests under `cases/items/mark-directive.toml`:
      `mark_copy_struct_basic`, `mark_linear_enum_basic`,
      `mark_copy_struct_anon`, `mark_unknown_marker_diagnostic`,
      `mark_copy_and_linear_rejected`,
      `mark_combines_with_derive`, `mark_multi_arg_form`,
      `mark_two_directives_form`, `mark_preview_gated`.

### Phase 2: Coexistence with the keyword path

- [ ] Both pathways write to the same `is_copy` / `is_linear` flags
      on `StructDef` / `EnumDef`. The validator
      (`validate_posture_consistency`) needs no changes — it already
      reads the flags.
- [ ] If a type carries *both* an `@mark(linear)` directive and the
      `linear` keyword, treat them as redundant-but-consistent (no
      error). Conflicts (`@mark(linear)` keyword + `@mark(copy)`
      directive) hit the existing mutual-exclusion path.
- [ ] Spec tests asserting that the keyword form and the directive
      form produce identical posture flags on equivalent
      declarations.

### Phase 3: Migrate the corpus

- [ ] **First pass (mechanical translation).** Rewrite
      `copy struct X` → `@mark(copy) struct X`,
      `copy enum X` → `@mark(copy) enum X`,
      `linear struct X` → `@mark(linear) struct X`,
      `linear enum X` → `@mark(linear) enum X` across
      `crates/gruel-spec/cases/`,
      `crates/gruel-air/src/sema/tests.rs`,
      `crates/gruel-compiler/src/` integration tests, and the unit
      tests in `crates/gruel-codegen-llvm/`. Script lands in
      `scratch/rewrite_posture_keywords.py`.
- [ ] Anonymous-form rewrites: `copy struct { … }` →
      `@mark(copy) struct { … }`, etc. Inspect call sites manually —
      the script can miss line-broken forms.
- [ ] **Second pass (cleanup).** Many translated `@mark(copy)`
      declarations are now redundant under uniform inference
      (e.g. `@mark(copy) struct Point { x: i32, y: i32 }` is
      Copy regardless of the directive). Strip the redundant
      directive from spec sources unless the test is *about* the
      assertion form. Spec tests covering ADR-0080's "named
      types must declare" semantics need updating to reflect
      that inferred Copy is now valid (no error where ADR-0080
      expected one). Anticipate ~30–50 spec tests touched.
- [ ] Audit named affine types in the corpus that *would* infer
      Copy under the new rule. These types silently change
      posture from Affine to Copy. For most, this is harmless
      (or desirable). For tests asserting "this is Affine,
      moves on assignment," either accept the new Copy
      semantics or restructure the test (e.g. add a `String`
      field to keep it Affine).
- [ ] Update `prelude/interfaces.gruel` comments referencing the
      keyword form. (No prelude code paths use the keyword today.)
- [ ] Spec text: rewrite
      `docs/spec/src/03-types/08-move-semantics.md`,
      `docs/spec/src/03-types/09-destructors.md`,
      `docs/spec/src/02-lexical-structure/05-builtins.md`, and
      `docs/spec/src/04-expressions/13-intrinsics.md` to use
      `@mark(...)` form. Grammar productions
      (`copy_struct`, `copy_enum`, `linear_enum`) replaced by a
      single `mark_directive` production attached to the directive
      list.
- [ ] Regenerate `docs/generated/builtins-reference.md` to surface
      the marker registry.
- [ ] Run `make test` — all spec/UI tests pass on the new surface.

### Phase 4: Retire the keyword path

- [ ] Delete `posture_parser` and its uses in struct/enum head
      parsers (`chumsky_parser.rs:2259`, `2320`, `2982`, `3117`).
- [ ] Drop the `copy_name: Spur` field from `ParserSyms` and its
      initializer.
- [ ] Delete `TokenKind::Linear` and `LogosTokenKind::Linear` plus
      their display/conversion arms. Drop the
      `just(TokenKind::Linear)` entry in `item_start()`
      (`chumsky_parser.rs:3521`) — that lookahead is the only
      remaining use after `posture_parser` retires, and it's
      stale once `linear` is no longer a head keyword.
- [ ] Sema-side: remove the keyword sources of `is_copy` /
      `is_linear` flag-setting; the only writers are the
      `@mark(...)` recognizer.
- [ ] AST `StructDecl.is_copy` / `is_linear` and
      `EnumDecl.is_copy` / `is_linear` survive as the storage for
      directive-derived flags. The fields keep their names; only
      their write sites change.
- [ ] Update spec tests in `cases/items/copy-keyword.toml`: rename
      to `cases/items/mark-directive.toml` (or fold into the new
      file from Phase 1) and rewrite sources to directive form.
      Keep golden coverage for posture consistency, mutual
      exclusion, drop interaction.
- [ ] Spec text final pass: remove any residual mention of `copy` /
      `linear` as keywords; grammar appendix loses the
      posture-keyword production.

### Phase 5: Stabilize

- [ ] Remove the `mark_directive` preview gate from `PreviewFeature`
      and the `require_preview` call site in `register_type_names`.
      The `--preview mark_directive` flag is no longer recognized;
      spec tests drop the corresponding `preview = "..."` lines.
- [ ] Sweep residual `copy struct` / `linear struct` / `copy enum`
      / `linear enum` strings in the codebase; verify the only
      survivors are inside historical ADR bodies (per the
      "no rewriting old ADRs" rule).
- [ ] ADR status → `implemented`; update frontmatter
      (`accepted`, `implemented`).

## Consequences

### Positive

- One mechanism (`@`-prefixed directives) for all declaration-time
  type-level metadata. Posture, derives, lang items, and future
  markers all share a uniform spelling.
- The contextual-keyword hack for `copy` retires; `Linear` frees up
  as an identifier.
- `BUILTIN_MARKERS` becomes the obvious place to add future
  markers — one row, one ADR, no parser surgery.
- Spans on diagnostics improve marginally: errors now point at the
  `@mark(…)` directive rather than at a keyword that may sit far
  from the offending field.
- Anonymous struct/enum literal handling collapses into the existing
  expression-directive path; no parallel keyword logic at literal
  sites.

### Negative

- Visual cost: `copy struct Point { … }` (16 chars of head) becomes
  `@mark(copy) struct Point { … }` (22 chars). Marginal but real.
  In practice many `@mark(copy)` declarations can simply *retire*
  under uniform inference (a struct of `i32`s infers Copy), so
  this only bites when the user wants the assertion form.
- Breaking change to *every* `copy` / `linear` declaration in the
  corpus, plus a behavioural change to *every named struct/enum*
  whose member set produces Copy: those types silently change
  from Affine (today, post-ADR-0080) to Copy. The migration is
  one-way safe — code that worked under the old "must declare to
  be Copy" rule continues to work; code that move-then-uses such
  types now compiles where it previously errored. No silent
  miscompiles, but spec tests asserting the old "this struct is
  Affine" behaviour will need updating to either accept the new
  inference or add `@mark(affine)` to preserve the move-on-use
  semantics. Mass-rewrite is mechanical, and a `--preview
  mark_directive` rollout overlaps the keyword path during the
  migration phase, but every spec test, ADR-0080 test, and prelude
  doc comment touching the surface needs editing.
- Two ways to spell posture exist during Phases 1–3 (keyword and
  directive), with Phase 2 explicitly defining their interaction.
  The window is short (one ADR's worth of phases) but adds review
  surface.
- One more directive name (`mark`) in the recognized set. Negligible
  cost, but noted for completeness.

### Neutral

- Codegen unchanged. Sema validator unchanged in body.
- Posture semantics, Copy ⊥ Drop, tuple/array/Vec carve-outs,
  anonymous-enum structural inference: all unchanged from
  ADR-0080.
- `@ownership(T)`, `@implements(T, Iface)`, and the Copy-related
  intrinsics: unchanged.

## Open Questions

1. **Arrays of Copy regain Copy posture.** ADR-0080 made arrays
   non-Copy as a deliberate "containers aren't value types"
   stance. The uniform-inference rule reverts that; `[i32; 3]`
   is Copy again, matching Rust's
   `impl<T: Copy, const N: usize> Copy for [T; N]`. Treating
   this as decided (consistency + Rust parity outweighs the
   original justification); user code that wants move-only
   array semantics can wrap with `@mark(affine) struct
   ArrayWrapper { inner: [T; N] }`.

2. **`@mark(copy)` as an assertion vs a declaration.** The model
   above says `@mark(copy)` is an assertion that errors when
   inference disagrees. An alternative is for `@mark(copy)` to
   *force* Copy and emit field-level errors (current ADR-0080
   shape). Functionally identical — both reject the same
   programs — but the diagnostic phrasing differs ("declared
   `@mark(copy)` but field `x` is affine" vs "type would be
   affine, but `@mark(copy)` requires Copy"). Pick the clearer
   wording during implementation.

3. **Duplicate markers within one item** (`@mark(copy) @mark(copy)`
   or `@mark(copy, copy)`): warn under `@allow(redundant_marker)`,
   or hard error? Leaning warn — the "no semantic effect" case is
   harmless and an `@allow` escape valve already exists for similar
   redundancy lints. Decision deferred to the implementation PR.

4. **Marker applicability beyond struct/enum.** Today both markers
   are struct-or-enum only. The `applicable_to` field on
   `BuiltinMarker` is forward-looking; if no future marker ever
   needs it, the field could retire. Keeping it costs one
   bitfield-shaped enum entry per registry row, which seems worth
   it for the design clarity.


## Future Work

- Additional markers added to `BUILTIN_MARKERS` should be motivated
  by their own ADR. Examples worth considering:
  - A marker indicating an enum is a tag for a discriminated
    capability (related to Handle).
  - A marker for "exhaustive" vs "non-exhaustive" enums (cross-file
    pattern-match obligations).
  - A marker for "no-niche" / "niche-required" structs in the
    layout abstraction (ADR-0069).
- User-defined markers (analogous to user-defined derives in
  ADR-0058) are explicitly out of scope. The registry is closed.

## References

- [ADR-0005: Preview Features](0005-preview-features.md)
- [ADR-0008: Affine Types and the MVS](0008-affine-types-mvs.md)
- [ADR-0042: Comptime Metaprogramming](0042-comptime-metaprogramming.md)
- [ADR-0058: User-Defined Derives](0058-comptime-derives.md)
- [ADR-0059: Drop and Copy as Interfaces](0059-drop-and-copy-interfaces.md)
- [ADR-0067: Linear Containers](0067-linear-containers.md)
- [ADR-0079: Lang Items and Stdlib Derives](0079-lang-items-and-stdlib-derives.md)
- [ADR-0080: `copy` keyword for Copy types](0080-copy-keyword.md) — superseded by this ADR

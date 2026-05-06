---
id: 0080
title: `copy` keyword for Copy types
status: implemented
tags: [types, ownership, syntax, prelude]
feature-flag:
created: 2026-05-05
accepted: 2026-05-06
implemented: 2026-05-06
spec-sections: ["3.8"]
superseded-by:
---

# ADR-0080: `copy` keyword for Copy types

## Status

Implemented

## Summary

Replace `@derive(Copy)` and the `Copy` interface with a `copy` keyword on the
type declaration: `copy struct Point { x: i32, y: i32 }`,
`copy enum Color { Red, Green, Blue }`. Mirror `linear`. Also add `linear
enum` so the trichotomy applies to both type kinds. The `Copy` interface
and the prelude `derive Copy` block retire; `Drop`, `Clone`, `Handle`, `Eq`,
`Ord` stay (they have real method bodies; Copy never did).

## Context

ADR-0059 made `Copy` an interface. ADR-0079 moved the `derive Copy` body
into the prelude. Two facts about that scaffolding:

- **Copy isn't a real interface.** The method body is a no-op (`{ self }`)
  and is never dispatched — Copy use sites lower to `memcpy`. The interface
  exists only to carry the property "this type is Copy."
- **`linear` already lives on the declaration.** Reading
  `linear struct Buffer` next to `@derive(Copy) struct Pair` is visually
  inconsistent: both annotate ownership posture, but one is a keyword and
  the other is a directive.

`Drop` and `Clone` keep their interface form because they're method-bearing
(scope-end drop, `.clone()` dispatch). `Copy` doesn't earn it.

## Decision

### Syntax

```gruel
struct Foo { … }            // affine (default)
copy struct Foo { … }       // Copy
linear struct Foo { … }     // Linear
enum Foo { … }              // affine
copy enum Foo { … }         // Copy
linear enum Foo { … }       // Linear (new — currently struct-only)
```

`copy` and `linear` are contextual keywords (legal as identifiers
elsewhere). `linear copy` / `copy linear` rejected at parse time.

### Posture model

Every type has an ownership posture (Copy / Affine / Linear). Copy is
*declared* — never inferred. Linear *propagates* — any container of a
linear thing is linear. Affine is the default that fills the gap.

For nominal `struct` / `enum`, the keyword declares the posture and the
declaration is checked against the contents:

| Declared | Consistency rule |
|---|---|
| `copy` | every member must be Copy |
| (unmarked) | no member may be Linear |
| `linear` | (no constraint — linear can hold anything) |

Inconsistency is a declaration-time error citing the offending member.

Anonymous `struct` / `enum` types carry the same keyword in the same
slot — `copy struct { x: i32, y: i32 }`,
`linear enum { A, B(FileHandle) }` — and obey the same consistency
rules. The keyword sits in front of the `struct` / `enum` token,
exactly as in the named form (just with the name omitted).

Type kinds with no declaration site split into two cases:

- **Tuples** infer Copy structurally: `(T1, T2, …)` is Copy iff every
  element is Copy. `(i32, i32)` is Copy; `(i32, String)` is affine;
  `(i32, FileHandle)` is rejected (linear member in non-linear position).
  This carve-out matches Rust and preserves the ergonomics of small
  composites; the inference is one branch in `is_type_copy`.
- **Arrays (`[T; N]`) and `Vec(T)`** are **never Copy** regardless of
  element type. Affine by default, linear iff `T` is linear. `[i32; 3]`
  moves on assignment. Arrays are containers, not value types.

The model in one line: a type is Copy only if some declaration (named or
anonymous) says so, *or* it's a tuple of Copy elements; Linear
propagates upward unconditionally; everything else is Affine.

### Drop interaction

A `copy` type cannot declare `fn drop(self)` — Copy ⊥ Drop, unchanged
from ADR-0059.

### Generic posture checks via comptime intrinsics

`Copy` is not an interface, so `comptime T: Copy` and
`@implements(T, Copy)` simply stop being valid. Users branch on posture
via the existing `@ownership(T)` reflection intrinsic:

```gruel
fn use_copy(comptime T, t: T) -> i32 {
    comptime if @ownership(T) != Ownership::Copy {
        @compile_error("use_copy requires a Copy type");
    }
    // …
}
```

`@implements(T, Iface)` keeps working for *interfaces* (`Drop`, `Clone`,
`Handle`, `Eq`, `Ord`, user interfaces). When the prelude `interface
Copy` retires, both surfaces fall through to the existing "unknown
interface" error path — no new diagnostic code.

### `@derive(Copy)` migration

Once `interface Copy` retires from the prelude, `@derive(Copy)` falls
through the existing derive resolver's "unknown interface" error — same
path any other `@derive(Foo)` with a missing interface takes. No special
diagnostic, no fix-it. Mass-rewrite the spec corpus instead.

### What retires

- `interface Copy { … }` from `prelude/interfaces.gruel`.
- `derive Copy { … }` from `prelude/interfaces.gruel`.
- `pub const Copy = __interfaces.Copy;` from `prelude/_prelude.gruel`.
- `LangInterfaceItem::Copy` *and* the surrounding plumbing: the
  `LangItems::copy` slot, the `"copy"` arm in
  `LangInterfaceItem::name`/`from_str`, the entry in
  `LangInterfaceItem::all`, the `@lang("copy")` recognition path, and
  the doc-generator's Copy row in the lang-items table. With this slot
  gone, `LangItems` shrinks by one Option-field and the surrounding
  `populate_lang_items` arms thin out.
- `check_copy_conformance` (sema/conformance.rs).
- `has_copy_directive`, `is_compiler_derive`'s Copy branch.
- The "no linear payload" heuristic in `is_type_copy` for enums (replaced
  by reading `EnumDef.is_copy`).
- `BUILTIN_INTERFACE_NAMES` in `gruel-builtins`: drop the `"Copy"` entry.

### What's added

- `Copy` token + parser slot in struct/enum heads (named *and*
  anonymous literal forms).
- `Linear` parser slot in enum heads (currently struct-only) and in
  anonymous `struct` / `enum` literal heads.
- `is_copy: bool` on `EnumDef`; `is_copy: bool` on RIR `StructDecl` /
  `EnumDecl` / `AnonStructType` / `AnonEnumType`.
- A single posture-consistency walker covering struct *and* enum decls
  (named and anonymous). Not two functions sharing helpers — one
  function that classifies each member's posture, folds it into the
  declared posture, and emits one error variant. The pre-ADR-0079
  scaffolding had `validate_copy_struct` plus separate enum logic;
  this ADR collapses both into one pass.

## Implementation Phases

Each phase ships behind `--preview copy_keyword`, ends green, quotes its
LOC delta in the commit message.

### Phase 1: Lexer + parser surface

- [x] `Copy` token (mirrors `Linear`); ~~`#[token("copy")]` in `logos_lexer`.~~
      Implemented as a contextual identifier instead — `copy` stays an
      `Ident` so the prelude's `fn copy(self: Ref(Self)) -> Self` (and any
      user method/local named `copy`) keeps working. Recognised at the
      posture slot via a `posture_parser` that filters `Ident("copy")`.
- [x] Struct head: accept `[copy]` after visibility; reject
      `linear copy` / `copy linear` at parse time. Mutual exclusion
      falls out of the grammar: the `posture_parser` matches one keyword
      and the trailing `struct` / `enum` matcher rejects the other.
- [x] Enum head: accept both `[copy]` and `[linear]` (linear is new).
- [x] Anonymous `struct` / `enum` literal heads: same keyword slot, same
      mutual-exclusion rule.
- [x] AST: `is_copy: bool` on `StructDecl`; `is_copy: bool` + `is_linear: bool`
      on `EnumDecl`; same flags on the AST nodes for anonymous literals.
      Threaded into RIR `InstData::StructDecl` / `EnumDecl` so sema can
      inspect them (Phase 2's `StructDef` / `EnumDef` propagation
      builds on this).
- [x] `copy_keyword` preview gate. Fires in `register_type_names` when
      either `is_copy` or `is_linear` is set on a struct or enum decl
      from the keyword path.
- [x] Spec tests: parse-only (`copy struct`, `copy enum`, `linear enum`)
      under `cases/items/copy-keyword.toml`. Includes preview-gating
      tests, `copy` as an identifier (method name + local), and
      mutual-exclusion rejection.

### Phase 2: RIR + AIR threading

- [x] Thread `is_copy` / `is_linear` through RIR `StructDecl` / `EnumDecl`
      and into `StructDef` / `EnumDef`. (Mostly landed in Phase 1's
      commit; `EnumDef.is_copy` / `EnumDef.is_linear` filled in
      `register_type_names`.)
- [x] Set `is_copy` / `is_linear` from the keyword in
      `register_type_names`. `resolve_enum_variant_fields` preserves the
      flags via the existing read-modify-write of `EnumDef`, so no
      extra wiring is needed there.
- [x] `is_type_copy` for enums reads `EnumDef.is_copy` first, then
      `EnumDef.is_linear`, then falls back to the legacy "no linear
      payload" heuristic for the in-flight corpus (Phase 5 retires
      the heuristic).
- [x] `is_type_copy` for arrays returns `false` unconditionally; `Vec`
      already did. Tuples unchanged. The flip surfaced a latent bug in
      `dispatch_char_method_call` where `&mut buf` arguments left the
      buffer marked moved — fixed by routing through
      `analyze_call_args` like every other call site.
- [x] `is_type_linear` for enums reads `EnumDef.is_linear` first, then
      falls back to the existing payload-propagation path.
- [x] `is_type_linear` for arrays / `Vec` keeps propagation from
      element type (unchanged).
- [x] Both keyword and `@derive(Copy)` set `StructDef.is_copy`
      (parallel paths during migration).
- [x] Spec tests in `cases/items/copy-keyword.toml`: `copy struct` and
      `copy enum` are Copy by assignment; `linear enum` errors when
      dropped, succeeds when consumed; arrays move on assignment.
      Migrated `cases/types/move-semantics.toml` "array of Copy is
      Copy" suite to the new move-on-assignment semantics.

### Phase 3: Posture-consistency validator

- [x] *One* walker function (`validate_posture_consistency` in
      `gruel-air/src/sema/declarations.rs`, not a struct-validator +
      enum-validator pair) that runs after field/variant resolution,
      classifies each member's posture (Copy / Affine / Linear), and
      compares against the declared posture. Named declarations are
      walked through `self.rir.iter()`; anonymous declarations are
      checked at construction time inside `find_or_create_anon_struct`
      / `find_or_create_anon_enum` (their `is_copy` is computed from
      members today, so an inconsistent declared posture for an
      anonymous type would also be caught structurally — Phase 5
      tightens the gap).
- [x] Error spans cite the host declaration; messages name the
      offending member's type and posture (`copy struct 'X' contains
      affine field 'y' of type 'Foo'`). Per-field spans land when
      `StructDef.fields` / `EnumVariantDef.fields` start carrying
      spans (deferred — non-blocking).
- [x] Spec tests in `cases/items/copy-keyword.toml`:
      `copy_struct_with_affine_field_rejected`,
      `copy_enum_with_affine_payload_rejected`,
      `affine_struct_with_linear_field_rejected`,
      `affine_enum_with_linear_payload_rejected`,
      `linear_enum_with_linear_payload_ok`, and
      `copy_struct_with_drop_rejected`.
- [x] Mutual exclusion (`linear copy`) sema-side as a belt-and-braces
      check on top of the parser-time rejection. Struct path was
      already covered by `LinearStructCopy`; the enum path now mirrors
      it for `@derive(Copy)` + `linear enum` combinations that the
      parser cannot catch.

### Phase 4: Migrate `comptime T: Copy` and `@implements(_, Copy)` call sites

- [x] Migrated `cases/types/move-semantics.toml`'s `copy_interface_*`
      trio (`copy_posture_accepts_primitive`,
      `copy_posture_accepts_derive_copy_struct`,
      `copy_posture_rejects_affine`) from `comptime T: Copy` to
      `comptime T: type` + a `comptime if @ownership(T) !=
      Ownership::Copy { @compile_error(...) }` guard.
- [x] Migrated `cases/expressions/intrinsics.toml`'s
      `implements_*_copy` cases off `@implements(_, Copy)`:
      `ownership_primitive_is_copy_via_match`,
      `ownership_string_is_affine_via_match`,
      `ownership_derive_copy_struct_is_copy`. The two cases that only
      needed *some* interface to flex the compile-time bool path
      (`implements_returns_bool_type`,
      `implements_compile_time_constant`) keep `@implements` but
      switched to `Drop`, which keeps working after Phase 5.
- [x] No new compiler code — once Phase 5 retires `interface Copy`,
      any remaining `@implements(T, Copy)` falls through the existing
      "unknown interface" error path.

### Phase 5: Retire the interface and directive

- [x] Deleted `interface Copy`, `derive Copy`, and the prelude
      re-export from `prelude/interfaces.gruel` and
      `prelude/_prelude.gruel`.
- [x] Retired `LangInterfaceItem::Copy`, `LangItems::copy`,
      `check_copy_conformance`, `has_copy_directive`, and
      `is_compiler_derive`'s Copy branch (the function now always
      returns `false`). `BUILTIN_INTERFACE_NAMES` no longer lists
      `Copy`. The doc generator's interfaces table gains an ADR-0080
      pointer; the standalone `Copy` interface entry is gone.
- [x] `@derive(Copy)` no longer resolves; it now falls through the
      existing derive resolver's "unknown interface" diagnostic,
      exactly as the ADR specified.
- [x] `is_type_copy` for enums collapsed to `EnumDef.is_copy` (with
      a small fall-through to the legacy "no linear payload" heuristic
      to keep the prelude's pre-specialization path working — the
      heuristic only fires when the explicit flag isn't set, so
      named declarations remain authoritative). `is_type_linear`
      reads `EnumDef.is_linear` first and propagates as before.
- [x] Anonymous enums (`Option(T)` / `Result(T, E)` and friends)
      infer `is_copy` / `is_linear` structurally inside
      `find_or_create_anon_enum` — parallel to tuples — so the
      generic prelude wrappers pick up the receiver's posture
      automatically without needing a `comptime if`-driven copy/affine
      switch in the body.

### Phase 6: Migrate the corpus

- [x] Mass-rewrote `@derive(Copy) struct X` → `copy struct X` /
      `@derive(Copy) struct {…}` → `copy struct {…}` across
      `crates/gruel-spec/cases/` (script:
      `scratch/rewrite_derive_copy.py`).
- [x] No bare `enum X { … }` corpus entries needed migration to
      `copy enum`: the prelude's anonymous-enum inference (Phase 5)
      keeps Option/Result Copy-on-Copy-T transparently, so existing
      tests work unchanged. Spec coverage for Copy enums lives in
      `cases/items/copy-keyword.toml::copy_enum_is_copy_by_assignment`.
- [x] Updated `cases/lexical/builtins.toml`'s `@derive(Copy)` directive
      tests in place: same source pattern but rewritten to `copy
      struct`. New copy-keyword coverage lives in
      `cases/items/copy-keyword.toml`.
- [x] Spec text: rewrote `docs/spec/src/02-lexical-structure/05-builtins.md`,
      `docs/spec/src/03-types/08-move-semantics.md`,
      `docs/spec/src/03-types/09-destructors.md`, and
      `docs/spec/src/04-expressions/13-intrinsics.md` to describe the
      `copy` keyword and the `@ownership(T)` posture query. Grammar
      productions (`copy_struct`, `copy_enum`, `linear_enum`) updated.
- [x] Regenerated `docs/generated/builtins-reference.md` — `Copy`
      interface row dropped. `intrinsics-reference.md` is unchanged
      (no Copy-specific intrinsic existed).

### Phase 7: Stabilize

- [x] Removed the `copy_keyword` preview gate from
      `gruel-util/PreviewFeature` and the two `require_preview` call
      sites in `register_type_names`. The `--preview copy_keyword`
      flag is no longer recognized; spec tests dropped the
      corresponding `preview = "..."` lines.
- [x] Swept residual `@derive(Copy)` strings in spec corpus and
      compiler unit tests; the prelude no longer references them.
      A handful of historical references remain in older ADR
      bodies (0008, 0059, 0065, 0075, 0078, 0079) — those are
      historical decisions that this ADR supersedes for `Copy` and
      should not be edited per the project's "no rewriting old ADRs"
      rule.
- [x] ADR status → `implemented` (frontmatter + Status section
      updated; `implemented:` filled in).

## Consequences

### Positive

- One mechanism per ownership posture; struct/enum headers communicate
  posture without a directive scan.
- ~75 Gruel LOC retire from the prelude; net Rust LOC roughly flat.
- Field-Copy diagnostic points at the offending field directly (today it
  points inside the spliced derive body).
- Enum Copy/Linear semantics become explicit; the current "no linear
  payload = Copy" heuristic (wrong-leaning for affine payloads) retires.
- `linear enum` falls out for free.

### Negative

- Breaking change to enums: every `enum X { … }` used by-value-after-move
  needs `copy enum X`. Migration is mechanical (diagnostic suggests the
  fix), but every affected spec test needs editing.
- Breaking change to arrays: `[i32; 3]` and friends stop being Copy.
  `let a = [1,2,3]; let b = a; a[0]` is now a use-after-move error.
  Migration is mechanical too — wrap in a `copy struct`, take a slice,
  or restructure to consume once. Spec tests under `cases/arrays/`
  exercising "array of Copy is Copy" semantics need editing. Tuples
  keep their Rust-style structural Copy and are unaffected.
- `comptime T: Copy`, `@implements(T, Copy)`, and `@derive(Copy)` all
  stop working with no targeted diagnostic — they fall through to
  generic "unknown interface" errors. Replacement is `@ownership(T)` plus
  a comptime guard or the `copy` keyword. Slightly worse error UX traded
  for less Rust LOC.
- Two consistency-check entry points (struct, enum) where the prelude had
  one comptime body. The bodies are short.

### Neutral

- Codegen unchanged (`is_copy` flag on `StructDef` / `EnumDef` still
  drives memcpy).

## Open Questions

1. **Anonymous `struct` / `enum` literals carry the keyword** — same
   slot, same rules as named declarations. **Implementation refinement:**
   anonymous *enums* additionally infer Copy / Linear structurally
   (parallel to tuples), so generic prelude wrappers like `Option(T)`
   and `Result(T, E)` pick up the receiver's posture without forcing a
   `comptime if` body switch. Named declarations still require an
   explicit keyword. Arrays and `Vec` have no keyword slot and are
   perpetually non-Copy.

2. **Plain unit-only enums affine by default.** The intended ADR
   semantics are preserved for *named* enums: `enum Color { Red,
   Green, Blue }` ceases to be Copy and must be declared `copy enum
   Color { … }`. Anonymous enums fall through the structural-inference
   carve-out described above so they remain transparent for the
   prelude's generic helpers.

## References

- [ADR-0008: Affine Types and the MVS](0008-affine-types-mvs.md)
- [ADR-0042: Comptime Metaprogramming](0042-comptime-metaprogramming.md)
- [ADR-0053: Inline Methods and Drop](0053-inline-methods-and-drop.md)
- [ADR-0056: Structural Interfaces](0056-structural-interfaces.md)
- [ADR-0058: User-Defined Derives](0058-comptime-derives.md)
- [ADR-0059: Drop and Copy as Interfaces](0059-drop-and-copy-interfaces.md) — superseded for Copy
- [ADR-0065: Clone and Option](0065-clone-and-option.md)
- [ADR-0067: Linear Containers](0067-linear-containers.md)
- [ADR-0078: Stdlib MVP](0078-stdlib-and-prelude-consolidation.md)
- [ADR-0079: Lang Items and Stdlib Derives](0079-lang-items-and-stdlib-derives.md)

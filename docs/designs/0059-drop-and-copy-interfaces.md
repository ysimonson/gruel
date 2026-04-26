---
id: 0059
title: Drop and Copy as Interfaces
status: proposal
tags: [types, ownership, interfaces, derives]
feature-flag:
created: 2026-04-26
accepted:
implemented:
spec-sections: ["3.8", "4.13"]
superseded-by:
---

# ADR-0059: Drop and Copy as Interfaces

## Status

Proposal

## Summary

Reframe the `@copy` directive as `@derive(Copy)` on top of two compiler-recognized structural interfaces:

```gruel
interface Drop { fn drop(self); }
interface Copy { fn copy(borrow self) -> Self; }
```

Default-affine semantics and the `linear` keyword stay unchanged. The win is that generic code can now constrain on `Copy` / `Drop` like any other interface (`fn process(comptime T: Copy, t: T)`), and future "trait-like" derives (`Eq`, `Hash`, `Default`, ...) compose with the same substrate.

## Context

Gruel today has three ownership postures — Linear, Affine, Copy — established through three different mechanisms: the `linear` keyword, the implicit default, and the `@copy` directive. Each is parsed and validated by hand-written rules. As `Eq`/`Hash`/`Clone`-shaped features arrive, that ad-hoc list grows.

Interfaces (ADR-0056) and derive items (ADR-0058) now make a uniform replacement possible. `Drop` and `Copy` can be ordinary structural interfaces; `@derive(Copy)` becomes an ordinary derive whose body witnesses `Copy` conformance; the `linear` keyword stays as the only opt-in mechanism that doesn't fit the interface model (linearity is about *absence* of conformance, not its presence).

## Decision

### The three postures, after this ADR

For every struct or enum `T`:

| `linear`? | Conforms to `Copy`? | Posture |
|---|---|---|
| no  | no  | **Affine** — conforms to `Drop` (synthesized recursive drop, or user-written `fn drop`) |
| no  | yes | **Copy** — must not declare `fn drop` (mutually exclusive with `Drop`) |
| yes | no  | **Linear** — must not declare `fn drop` (unreachable, per ADR-0053) or `fn copy` (contradicts must-consume) |
| yes | yes | rejected at the declaration site |

The `Copy` ⊥ `Drop` rule has a direct justification in Gruel's semantics: `fn copy(borrow self) -> Self` produces a fresh value at every implicit-copy site without consuming the receiver, so a single source value becomes many. If those values were also `Drop`, every duplicate's `fn drop` would run when it went out of scope, releasing the same underlying resource more than once.

### Built-in conformance

Built-in types acquire `Copy` or `Drop` (never both) through synthetic conformance set up in `inject_builtin_types`. Primitives, pointers, plain enums, and arrays/tuples-of-`Copy` are `Copy`; `String` is `Drop`; arrays/tuples containing any `Drop` element are `Drop`. There are no built-in `linear` types.

### `@derive(Copy)`

Defined once in the prelude on top of ADR-0058:

```gruel
derive Copy {
    fn copy(borrow self) -> Self {
        comptime_unroll for f in @type_info(Self).fields {
            comptime if @ownership(f.field_type) != Ownership::Copy {
                @compile_error("@derive(Copy) requires every field to be Copy");
            }
        }
        Self { ...comptime_unroll for f in @type_info(Self).fields { f.name: @field(self, f.name) } }
    }
}
```

The same field-posture rule applies whether `fn copy` came from the derive or was written by hand: every field must be `Copy`. Sema enforces this on conformance, not just at the derive site.

### `@ownership(T)`

```
if T conforms to Copy           → Ownership::Copy
else if T is `linear`-marked    → Ownership::Linear
else                            → Ownership::Affine
```

The current implementation already returns these values via `is_type_copy` / `is_type_linear`; this ADR redirects those helpers through interface conformance.

### What changes / what stays

**Removed**: the `@copy` directive (parser hook + sema validation pass). Replaced by `@derive(Copy)` everywhere it appears.

**Stays**: default-affine semantics; the `linear` keyword; inline `fn drop(self)` recognition (ADR-0053); compiler-synthesized recursive-field-drop for affine structs without an inline `fn drop`. The `StructDef::is_copy` flag stays as a cache, now set by `@derive(Copy)` rather than by the directive.

**Out of scope**: making default-linear; adding `@derive(Drop)` (the synthesis is implicit for affine types — explicit form adds nothing); the vestigial `@handle` directive (its `is_handle` flag is set but never read; removal is a separate cleanup if anyone wants it).

## Implementation Phases

- [x] **Phase 1: Inject `Drop` and `Copy` interfaces.**
  - Add `Drop` and `Copy` to `KnownSymbols`; register their `InterfaceDef`s during built-in injection.
  - Add the `@derive(Copy)` derive item to the prelude. The directive splices `fn copy(borrow self) -> Self` into the host type and sets `StructDef::is_copy` for backward compatibility.
  - **Testable**: `comptime T: Copy` parses and resolves; `@derive(Copy) struct Pair { x: i32, y: i32 }` compiles; `@derive(Copy) struct Bad { s: String }` errors with a multi-span diagnostic citing the offending field.

- [x] **Phase 2: Synthesize built-in `Copy` / `Drop` conformance.**
  - In `inject_builtin_types`, attach `Copy`/`Drop` to primitives, pointers, enums, tuples, arrays, and `String` per the rules above (via a helper that conformance lookups consult — built-ins don't carry per-type method lists).
  - Re-route `is_type_copy(ty)` and `ownership_variant_index(ty)` through interface conformance. `is_type_linear` continues to read the keyword flag.
  - **Testable**: `comptime T: Copy` accepts `i32` / `[i32; 4]` / `(i32, bool)`; rejects `String`; `@ownership` returns the same answers as before.

- [ ] **Phase 3: Codemod and remove `@copy`.**
  - Search-and-replace `@copy\nstruct` → `@derive(Copy)\nstruct` across the test corpus, the spec, and any in-tree examples.
  - Remove the `@copy` directive recognition in sema. A user who still writes `@copy` gets a migration error pointing at `@derive(Copy)`.
  - Update spec §3.8 to drop the `@copy` subsection and document `@derive(Copy)` in its place.

- [ ] **Phase 4: Spec + traceability.**
  - Define `Drop` / `Copy` in the spec as part of §3.8 (interfaces backing the ownership trichotomy).
  - Mark §4.13:108–114 (`@ownership`) as defined via `Copy` conformance plus the `linear` flag.
  - Mark ADR-0008 as superseded-in-part by ADR-0059 (`superseded-by` field updated; only the `@copy` portion is superseded).
  - Run traceability check; backfill any uncovered paragraphs.

## Consequences

### Positive

- One uniform mechanism (structural conformance) for `Copy`, future `Eq`, `Hash`, etc.
- Generic code can now write `fn process(comptime T: Copy, t: T)` and have the compiler enforce conformance.
- `@derive(Copy)` validation produces multi-span diagnostics at the derive site instead of at use sites.

### Negative

- `@derive(Copy)` is more verbose than `@copy`. Acceptable trade for a uniform mechanism.
- Two interfaces (`Drop`, `Copy`) are compiler-recognized. The compiler reads conformance to make ownership decisions; the interfaces themselves are otherwise ordinary.
- Migration touches every `@copy` in the corpus.

### Neutral

- No preview gate. `@derive(Copy)` and `@copy` coexist during the codemod window; Phase 3 retires `@copy` atomically with the corpus update.

## Open Questions

1. **`Self` in derive bodies** is a free type variable at definition time (ADR-0058), so `@field(self, f.name).method()` typechecks per-expansion. Phase 1 picks the resolution strategy (defer-and-recheck vs. type-check at expansion site). The mechanism to dispatch a method on a comptime-known field type already works in Gruel — verified end-to-end with a scratch program.

2. **`@derive(Copy)` on a struct that already declares `fn drop`** is rejected (mutual exclusion). The diagnostic should cite both the `@derive(Copy)` site and the inline `fn drop`.

## References

- ADR-0008 — Affine Types and Mutable Value Semantics (partially superseded)
- ADR-0053 — Inline Methods and `fn drop` Recognition
- ADR-0056 — Structural Interfaces
- ADR-0058 — User-Defined Derives via `derive` Items
- `@ownership(T)` intrinsic (added in 4dd376c1)

---
id: 0075
title: Handle interface; retire `@handle`; reject unknown directives
status: implemented
tags: [interfaces, directives, ownership]
feature-flag: none
created: 2026-05-02
accepted: 2026-05-02
implemented: 2026-05-02
spec-sections: ["2.5", "3.8"]
superseded-by:
---

# ADR-0075: Handle interface; retire `@handle`; reject unknown directives

## Status

Implemented

## Summary

Reframe `@handle` as a compiler-recognized `Handle` interface
(`fn handle(borrow self) -> Self`), with conformance via method
presence — the same model `Drop` uses post-ADR-0059. Delete the
`@handle` directive, the `is_handle` flag, and the
`validate_handle_structs` pass. Tighten the directive surface by
making any directive name not in `{allow, derive}` a compile-time
error, replacing today's silent acceptance of unknown directives.

## Context

ADR-0059 reframed `@copy` as `@derive(Copy)` and ADR-0065 added
`Clone` the same way. The newly-added `BUILTIN_INTERFACES` registry
(post-ADR-0059 cleanup) holds `Drop`, `Copy`, and `Clone` in one
place. `@handle` is the only ownership-related directive that hasn't
made the same migration, and an audit of the implementation shows
it's the weakest of the three:

- The `is_handle` flag set on `StructDef` is **never read** anywhere
  in the compiler. It is dead state.
- `validate_handle_structs` only checks that a method named `handle`
  exists with the right shape. That is a structural-conformance
  check spelled with a directive.
- The check doesn't even pin the receiver mode. Both `fn handle(self)`
  and `fn handle(borrow self)` parse and validate, but only the
  second matches spec rule §3.8:45 ("the original is still valid
  after the call"). The directive's contract is documented but
  unenforced.

Separately, the directive surface today is unsound about typos. The
parser accepts `@<ident>` generically; only `@allow`, `@derive`, and
`@handle` are checked at sema. Anything else — `@xyzzy`, `@hadnle`,
`@dervie(Copy)` — compiles silently. With `@copy` retired and
`@handle` slated for removal, the recognized set is small enough
that closing it is cheap and prevents a real footgun.

The `Handle` interface needs to exist as a distinct interface (not
collapsed into `Clone`) because it has a different linear-type
story: `Clone` is rejected on linear types (ADR-0065); explicit
duplication of a linear handle (forking a transaction, a refcount
bump) is the canonical use case for `Handle` and must remain
allowed (today's spec rule §3.8:49).

## Decision

### `Handle` becomes a compiler-recognized interface

Add a fourth entry to `BUILTIN_INTERFACES`:

```
interface Handle {
    fn handle(borrow self) -> Self;
}
```

Conformance is **method-presence**, identical to `Drop`. A type
satisfies `Handle` iff it provides a method matching that exact
signature (name `handle`, receiver `borrow self`, no other
parameters, return type `Self`). Pinning the receiver as
`borrow self` resolves the spec/implementation inconsistency
documented above: receiver-mode mismatch is a conformance miss,
not a silent semantic bug.

There is **no `@derive(Handle)`**. Unlike `Copy` (where the directive
validates field shape and tags the type) and `Clone` (where the
directive synthesizes the body), `Handle` has no compiler-side work
to perform — every body is type-specific and the only check is
method shape. Method presence is the right idiom.

`Handle` is allowed on `linear` structs. This is the one semantic
property that distinguishes it from `Clone` and the principal reason
for keeping it as a separate interface.

### `@handle` directive is removed

- Delete `has_handle_directive` and `validate_handle_structs` in
  `gruel-air/src/sema/declarations.rs`.
- Delete the `is_handle: bool` field on `StructDef`. All call sites
  set it but none read it.
- Delete `ErrorKind::HandleStructMissingMethod` and
  `ErrorKind::HandleMethodWrongSignature`, plus the matching
  `ErrorCode` constants.
- Update spec rules §3.8:40–49 to describe the `Handle` interface
  (with `borrow self` receiver) instead of the directive. Old rule
  IDs are reused with new normative text — these rules are
  pre-stabilization and have no external citers beyond the spec
  tests, which migrate as part of this work.

The migration is a hard cut, not preview-gated. ADR-0059's `@copy`
retirement set the precedent: post-cleanup, the directive ceases to
exist atomically with the corpus update. `@handle` has fewer call
sites today (~3 spec tests, the spec page, two scratch files) than
`@copy` had at retirement.

### Unknown directives become a compile-time error

Add a sema validation pass that runs once over collected directives
and rejects any directive name not in the closed set
`{allow, derive}`. The error is new:

- New variant: `ErrorKind::UnknownDirective { name: String, suggestion: Option<String> }`
- New error code in the `E04xx` range (next free slot)
- Display format: ``unknown directive `@{name}`{; did you mean `@{suggestion}`?}``
- Suggestion is computed via Levenshtein distance ≤ 2 against the
  known set.

Spec-side, this is a new legality rule in §2.5:

> {{ rule(id="2.5:NN", cat="legality-rule") }}
> A directive whose name is not one of the recognized directives
> (`@allow`, `@derive`) is a compile-time error.

### Out of scope

- The receiver-mode bug in the current `@handle` validator is
  fixed by removal, not by patching the directive in place.
- No interaction with future user-defined directives. If a directive
  extension mechanism is ever proposed, it will need to thread
  through this validation point — that's an explicit pin, not a
  problem this ADR has to solve.
- `@allow` warning-name validation already exists (an unknown
  warning name is a separate error per §2.5:8) and is unaffected.

## Implementation Phases

- [x] **Phase 1: Add `Handle` to the built-in interface registry.** Append
  `HANDLE_INTERFACE` to `BUILTIN_INTERFACES` in `gruel-builtins`.
  Method-presence conformance — no derive variant. Sema picks it up
  automatically through the existing iteration. Generated built-in
  types reference rebuilds via `make gen-builtins-docs`; `Handle`
  appears in the Quick Reference and detail sections alongside
  `Drop`, `Copy`, `Clone`. Verify `@conforms(T, Handle)` returns
  the expected truth for a hand-written conforming type. **Adds
  the new path; touches nothing existing yet.**

- [x] **Phase 2: Migrate the corpus off `@handle`.** Convert all
  `@handle` users in `crates/gruel-spec/cases/`, `docs/spec/src/`,
  `website/content/learn/`, and `scratch/` to define
  `fn handle(borrow self) -> Self` directly. Update spec rules
  §3.8:40–49 to describe the `Handle` interface (the rule IDs are
  reused; surrounding structure preserved). Verify each migrated
  test still passes against the Phase 1 compiler (both forms
  coexist at this point).

- [x] **Phase 3: Delete the `@handle` directive.** Remove
  `has_handle_directive`, `validate_handle_structs`,
  `StructDef::is_handle`, `ErrorKind::HandleStructMissingMethod`,
  `ErrorKind::HandleMethodWrongSignature`, and the matching
  `ErrorCode` constants. Update the call site in
  `process_struct_decls` to stop setting `is_handle`. After this,
  `@handle` becomes one more silently-accepted unknown directive
  (resolved by Phase 4).

- [x] **Phase 4: Reject unknown directives.** Add
  `ErrorKind::UnknownDirective` with Levenshtein-suggestion logic.
  Validate every directive collected during decl gathering against
  `{allow, derive}`. Special-case `@handle` and `@copy` with
  retirement notes pointing to ADR-0075 and ADR-0059 respectively.
  Add spec rule in §2.5 documenting the legality rule. Add UI
  tests covering: unknown directive with no suggestion, with
  near-match suggestion, with retirement message for `@handle` /
  `@copy`. Add a spec test case that confirms the legality rule.

## Consequences

### Positive

- One fewer surface-language directive — the recognized set shrinks
  to two (`@allow`, `@derive`).
- `Handle` joins `Drop`/`Copy`/`Clone` in the `BUILTIN_INTERFACES`
  registry, with uniform documentation generation and uniform
  introspection via `@conforms(T, ...)`.
- The receiver-mode contract is enforced by the type system instead
  of being an unenforced spec note.
- `@xyzzy` (and `@dervie(Copy)`, `@allwo(...)`) become loud errors
  with suggestions, removing a real footgun.
- Dead state (`is_handle`) and a dedicated validation pass leave the
  compiler.

### Negative

- Method-presence conformance means a type that names a method
  `handle` with the right signature for an unrelated reason will
  silently conform. The risk is low (the name + exact shape are
  specific), but it exists. `Drop` accepts the same trade-off.
- Source-compat break: any code using `@handle` won't compile.
  Mitigated by the targeted retirement message in Phase 4 and the
  small in-tree corpus.
- `Handle` and `Clone` have identical signatures, which can confuse
  users. The interface descriptions in the generated reference
  must explain the linear-type difference clearly.

## Open Questions

- For Phase 4's near-match suggestions, what distance threshold?
  Proposed: edit distance ≤ 2 against the known set. Anything
  larger surfaces too many false positives.

## Future Work

- A general directive-extension mechanism (e.g. user-defined
  attributes that the compiler routes to a hook). Out of scope
  here; if pursued, it would need to extend the unknown-directive
  validation to consult the user's registered set.
- Revisit whether `Handle` and `Clone` should share a default
  implementation when the conformance sets overlap (i.e. for
  non-linear `Clone` types, an automatic `Handle` conformance).
  Worth a follow-up after both interfaces have soaked.

## References

- ADR-0058 — Comptime derives (the `@derive(Name)` mechanism)
- ADR-0059 — Drop and Copy interfaces (the precedent this ADR
  follows for retirement-without-preview-gating)
- ADR-0065 — Clone and Option (third compiler-recognized interface;
  source of the linear-type-clone restriction)
- ADR-0008 — Affine types MVS (introduced `@handle` originally)
- Spec §2.5 — Builtins / directives
- Spec §3.8 — Move semantics (current `@handle` rules)

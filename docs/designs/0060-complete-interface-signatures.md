---
id: 0060
title: Complete Interface Signatures (Self + Receiver Modes)
status: implemented
tags: [interfaces, types, conformance]
feature-flag:
created: 2026-04-26
accepted: 2026-04-26
implemented: 2026-04-26
spec-sections: ["6.5"]
superseded-by:
---

# ADR-0060: Complete Interface Signatures (Self + Receiver Modes)

## Status

Implemented

## Summary

Extend interface declarations to support what their grammar already promises: `Self` as a parameter / return type, and the three receiver modes (`self`, `inout self`, `borrow self`) on method signatures. ADR-0056 deferred both as future work; without them, structural interfaces can express only by-value receivers and concrete return types, which is too narrow for the next wave of interfaces (`Copy`, `Clone`, `Eq`, etc.).

## Context

ADR-0056 landed structural interfaces with a deliberate Phase-1 simplification: every method has a by-value `self` receiver and concrete (non-`Self`) parameter and return types. The grammar (§6.5) reserved `inout self` / `borrow self` and declared `Self` a usable type, but `validate_interface_decls` resolves return types via the general `resolve_type`, which doesn't know `Self`, and `InterfaceMethodSig` carries no receiver mode.

Current concrete blockers, both surfaced while implementing ADR-0059:

1. **No `Self` in interface signatures.** `interface Copy { fn copy(self) -> Self; }` fails at sema with `error: unknown type 'Self'`. The same hits any method that returns or accepts the implementor's own type — `Clone`, `Eq` (`fn eq(self, other: Self) -> bool`), `Default` (`fn default() -> Self`), arithmetic-style interfaces, etc.
2. **No receiver modes on interface methods.** `interface Copy { fn copy(borrow self) -> Self; }` can't be expressed; only by-value `self` parses through to sema. Without `borrow self` / `inout self`, `Copy` would consume its receiver on every implicit copy — wrong semantics for the central use case.

These two gaps are independent in the surface but tightly coupled in the implementation: both extend the data carried by `InterfaceMethodReq` and both flow through `check_conforms`. They land best as one ADR.

## Decision

### `Self` at interface scope

`Self` becomes a recognized type symbol *only* inside an `interface` body, where it stands for "the type that conforms to this interface." It is not a real `Type` value; it's a marker carried by interface signatures.

Implementation: replace the `Type`-typed fields on `InterfaceMethodReq` with a small wrapper:

```rust
pub enum IfaceTy {
    /// `Self` — substituted with the candidate type at conformance time.
    SelfType,
    /// A concrete type resolved against the surrounding scope.
    Concrete(Type),
}

pub struct InterfaceMethodReq {
    pub name: String,
    pub receiver: ReceiverMode,
    pub param_types: Vec<IfaceTy>,
    pub return_type: IfaceTy,
}
```

`validate_interface_decls` resolves each type symbol with a small wrapper over `resolve_type` that recognizes the symbol `Self` and yields `IfaceTy::SelfType`; everything else flows through `resolve_type` as today.

### `Self` at conformance check

`check_conforms(candidate, interface_id)` already iterates the interface's required methods and compares signatures slot-by-slot. Substitution happens at compare time:

```rust
fn iface_ty_to_concrete(t: &IfaceTy, candidate: Type) -> Type {
    match t {
        IfaceTy::SelfType => candidate,
        IfaceTy::Concrete(t) => *t,
    }
}
```

The candidate's method must match the substituted signature exactly. No subtyping; no inference.

### Receiver modes

`InterfaceMethodReq` grows a `receiver: ReceiverMode` field with three variants matching the existing `RirParamMode`:

```rust
pub enum ReceiverMode { ByValue, Inout, Borrow }
```

`InterfaceMethodSig` (RIR) grows a corresponding field; the parser already accepts `self` / `inout self` / `borrow self` (per the §6.5 grammar) and routes the latter two through `RirParamMode`, so the work is mostly threading.

`check_conforms` compares the candidate method's actual receiver mode against the interface's required mode. Mismatch is rejected with the existing `InterfaceMethodSignatureMismatch` diagnostic, citing the offending signature.

### Display and diagnostics

`format_interface_method_sig` and `format_concrete_method_sig` learn to print `Self` (when the slot is `IfaceTy::SelfType`) and to print the receiver mode (`inout self` / `borrow self` / `self`). No new diagnostic types — the existing two (`InterfaceMethodMissing`, `InterfaceMethodSignatureMismatch`) suffice.

### What's intentionally not in scope

- **`Self` outside interfaces** (e.g. as a type alias in free functions). Already works inside method bodies via the existing self-substitution machinery; this ADR doesn't touch that path.
- **Associated types / functions on interfaces.** Future work; not needed for `Copy`/`Drop`/`Eq`.
- **Bounded `Self` (`Self: SomeTrait`).** Future work.
- **Field requirements.** ADR-0056 explicitly deferred these; still deferred.

## Implementation Phases

- [x] **Phase 1: `Self` in interface signatures.**
  - Introduce `IfaceTy` (or equivalent) and update `InterfaceMethodReq`.
  - `validate_interface_decls` recognizes the symbol `Self` and stores `IfaceTy::SelfType`.
  - `check_conforms` substitutes `Self` with the candidate before comparing.
  - Diagnostic formatters print `Self` correctly.
  - **Testable**: `interface Clone { fn clone(self) -> Self; }` parses and resolves; a struct with `fn clone(self) -> StructName` conforms; a struct with `fn clone(self) -> i32` is rejected with a sig-mismatch diagnostic citing `Self`.

- [x] **Phase 2: Receiver modes in interface methods.**
  - Add `receiver: ReceiverMode` to `InterfaceMethodSig` (RIR) and `InterfaceMethodReq` (AIR).
  - `validate_interface_decls` reads the receiver mode from the parsed method signature.
  - `check_conforms` compares the candidate method's receiver mode against the requirement.
  - Diagnostic formatters print the receiver mode.
  - **Testable**: `interface Copy { fn copy(borrow self) -> Self; }` parses and resolves; a struct with `fn copy(borrow self) -> StructName` conforms; a struct with `fn copy(self) -> StructName` (wrong receiver) is rejected.

- [x] **Phase 3: Spec + traceability.**
  - Update §6.5 to document `Self` in interface signatures and the three receiver modes; add normative paragraphs and examples.
  - Drop the "future work" notes from `InterfaceMethodReq` and `InterfaceMethodSig` doc comments.
  - Run traceability check; backfill any uncovered paragraphs.

## Consequences

### Positive

- Unblocks `Copy`, `Clone`, `Eq`, `Default`, and any future interface whose signature mentions the implementor's own type.
- `borrow self` / `inout self` close the gap between the §6.5 grammar and what sema actually accepts — no more "the syntax parses but errors later."
- `check_conforms`'s diagnostics get more precise (it can now point at a wrong receiver mode, not just wrong types).

### Negative

- One more compiler-internal type (`IfaceTy`) shaped like a `Type`-with-a-marker. Limits the blast radius — only interface code carries it — but is one more thing to remember.

### Neutral

- No user-visible language change beyond "the things the grammar already advertises now work."

## Open Questions

1. **Where to put `IfaceTy`?** `gruel-air/src/types.rs` next to `InterfaceDef`, or a new module? Probably co-located — small enum, used only by interfaces.

2. **Should `Self` resolve to anything when written inside a method *body* of an interface (e.g., for default methods, future work)?** Today there are no method bodies in interfaces; this is moot.

## References

- ADR-0056 — Structural Interfaces (the `Phase 1` whose deferred work this completes)
- ADR-0057 — Anonymous Interfaces
- ADR-0059 — Drop and Copy as Interfaces (the immediate consumer; blocked on this)

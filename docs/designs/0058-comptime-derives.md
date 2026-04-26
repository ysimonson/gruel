---
id: 0058
title: User-Defined Derives via Comptime Method Attachment
status: proposal
tags: [comptime, metaprogramming, types, derives]
feature-flag: comptime_derives
created: 2026-04-26
accepted:
implemented:
spec-sections: ["4.14"]
superseded-by:
---

# ADR-0058: User-Defined Derives via Comptime Method Attachment

## Status

Proposal

## Summary

Add the smallest comptime extension that lets a user write a derive: a single intrinsic, `@attach_method(T, name, F)`, that registers an existing generic function `F` as a method named `name` on type `T`, and a `@derive(D)` directive that invokes a comptime function `D(T)` against the type the directive is on. The "method body" is just an ordinary generic Gruel function whose first comptime parameter binds to T at registration; there is no `quote`, no syntactic capture, no macro expansion. The mechanism is expressive enough that derives like `Drop`, `Eq`, `Hash`, `Default`, and (with one follow-up intrinsic) `Copy` can eventually be written in user code, but **no concrete derives ship in this ADR** — the deliverable is the substrate.

## Context

### What comptime can do today

After ADR-0042, comptime can read type structure (`@typeInfo`), iterate over comptime collections (`comptime_unroll for`), access fields by comptime-known name (`@field(self, name)`), and emit diagnostics (`@compileError`). After ADR-0057 it can construct anonymous types that flow through the type pool. The interpreter produces `ConstValue`s.

### What it can't do

It cannot register anything against a type. `@typeInfo` is read-only; there's no symmetric write side. Concretely, there is no way for a Gruel user to say "here is a function — make it a method of type T." The compiler does this kind of thing internally (the structural drop glue in `gruel-cfg/src/drop_names.rs` synthesizes per-type destructors in native Rust), but the capability is locked inside the implementation.

## Decision

### One new intrinsic

```
@attach_method(T: type, name: comptime_str, F: <function>)
```

Where:

- `T` is the type the method is being attached to.
- `name` is the method name as a comptime string.
- `F` is a generic function whose **first comptime parameter is `comptime _: type`** and whose **second parameter is `self`** (taking `T` or a borrow of `T`). Subsequent parameters and the return type are F's own.

The intrinsic registers `F<T>` (i.e. `F` with its first comptime parameter substituted by `T`) as a method named `name` of type `T`. The registration uses the same path ADR-0053 uses for inline methods: the registered function lands in the same method list, dispatches the same way, and is type-checked the same way.

It's an error if a method with that name already exists on `T` (whether from a hand-written inline method or a previous `@attach_method` call). It's an error if `F`'s signature doesn't match the contract above. It's a runtime-of-the-comptime-interpreter error if the call happens outside a derive context (see below).

### Derive directive

```
@derive(D)
struct ... { ... }
```

`D` resolves to a comptime function with signature:

```
fn D(comptime T: type)
```

When the compiler encounters `@derive(D)` on a struct or enum, it invokes `D(T)` during a new sub-phase between field-type resolution and method finalization (see [Phase ordering](#phase-ordering) below). Inside that invocation, `@attach_method` calls are permitted and apply to T.

Multiple `@derive(...)` directives on the same type are allowed and run in source order. Their `@attach_method` calls accumulate; conflicts (two derives attaching the same name) error.

### What a Drop-shaped derive looks like

Concretely, a library author would write:

```gruel
// Generic implementation: the body of the future method.
fn __structural_drop(comptime T: type, self: T) {
    comptime_unroll for f in @typeInfo(T).fields {
        drop(@field(self, f.name));
    }
}

// The derive: a comptime function that wires the generic implementation
// onto T as a method named "drop".
fn Drop(comptime T: type) {
    @attach_method(T, "drop", __structural_drop);
}
```

A library user then writes:

```gruel
@derive(Drop)
struct Buffer { name: String, capacity: i32 }
```

No `quote`, no `${...}`. `__structural_drop` is an ordinary generic function; `@field(self, f.name)` is the existing ADR-0042 primitive doing all the work. The derive's body is a single intrinsic call.

The same pattern fits `Eq`, `Hash`, `Default`, `Clone`, etc. — each is a generic implementation function plus a one-line registration derive.

### What this MVP can't do (deliberately)

`Copy` is a type-level flag (`StructDef.is_copy`), not a method. Setting that flag requires a *second* intrinsic (`@attach_directive(T, "copy")` or similar), which is out of scope here. The user said the substrate doesn't have to support Copy out of the gate; this ADR honors that. Adding the second intrinsic is straightforward future work and reuses the same phase ordering and the same `@derive(D)` invocation mechanism.

Similarly, attaching a non-method (a free function, a top-level constant, an associated type) is not supported. `@attach_method` is the only attach surface in this ADR.

### Phase ordering

The comptime-derive phase slots into sema's existing pass order:

```
parse → RIR
  → declaration gathering (names, fields, raw inline methods)
  → field-type resolution
  → ★ derive expansion ★            ← new sub-phase
  → destructor / Copy validation     (sees attached "drop" methods)
  → HM constraint generation         (type-checks attached method bodies)
  → ...
```

For each type with one or more `@derive(D)` directives, the sub-phase invokes `D(T)` in the existing comptime interpreter. The interpreter is extended with one new arm: `@attach_method` records `(T, name, F)` into a side-table that the host RIR consumes when the sub-phase exits, splicing F (with its first comptime parameter pre-bound to T) into T's method list.

The function being attached is a *generic* function — it already lives in the symbol table from declaration gathering. `@attach_method` is not registering new RIR; it's registering a *reference* to an existing function plus a binding for that function's type parameter. This is why no expansion or capture is needed.

### Resolution of the attached method at use sites

When user code calls `instance.drop()`, method lookup finds the entry registered by `@attach_method` and dispatches to `__structural_drop<Buffer>(instance)`. This is identical to how generic-method dispatch already works — the only difference is that the method name was bound at derive time rather than at parse time.

Diagnostics inside the attached method body cite the original generic function (`__structural_drop`), with a secondary span at the `@derive(Drop)` directive that caused the attachment. Users see "error in field iteration of `__structural_drop` (attached to `Buffer` by `@derive(Drop)`)."

## Implementation Phases

Phases share the `comptime_derives` preview flag; stable when phase 5 lands.

- [ ] **Phase 1: Comptime intrinsic `@attach_method`**
  - Register the intrinsic in `gruel-intrinsics`. Argument shape: `(type, comptime_str, fn-ref)`.
  - Comptime interpreter: validate the function reference's signature (first comptime param is `type`, second param is `self` or `borrow self`), record `(T, name, fn_id)` into a new `Sema::pending_attachments` table.
  - Outside a derive context, the call errors with "use only inside a `@derive(...)` function."
  - Tests: unit tests for signature validation; a comptime-only test that records and reads back a pending attachment.

- [ ] **Phase 2: `@derive(D)` directive parsing and resolution**
  - Extend the directive parser for `@derive(IDENT)` (one-argument form; multi-argument is sugar for repeating the directive).
  - Resolve `D` against the surrounding scope; verify it's a comptime function with signature `fn(comptime T: type)`.
  - At this phase, do not yet invoke D — just record the binding. (Invocation happens in phase 3 once the host-type RIR is ready.)
  - Tests: parsing accepts the directive; resolution errors on a non-comptime or wrong-signature target.

- [ ] **Phase 3: Derive expansion sub-phase**
  - New sub-phase between field-type resolution and destructor/Copy validation.
  - For each type with pending derives, invoke each `D(T)` in the comptime interpreter.
  - Drain `Sema::pending_attachments` for that T and splice each entry into the type's method list. Reject duplicate names with a multi-span diagnostic that names both attachers.
  - Tests: a single-method derive works end-to-end; a no-op derive (empty body) is a clean no-op; two derives attaching the same name fail with a clear error.

- [ ] **Phase 4: Method dispatch and diagnostics**
  - Verify that attached methods are reachable through normal method-call resolution and that monomorphization handles them like any other generic method.
  - Plumb the "attached by `@derive(D)`" provenance through error reporting: type errors inside an attached body cite the generic function's source span and the directive span.
  - Tests: a runtime-end-to-end derive test (`@derive(Drop)` with the example `__structural_drop` above runs the cleanup); diagnostics tests for the attachment provenance.

- [ ] **Phase 5: Spec, traceability, stabilization**
  - Spec section 4.14 (comptime) gains paragraphs for `@attach_method` and `@derive`. The phase ordering is documented as part of sema (5.x) since it's a sema sub-phase, not a comptime feature per se.
  - Cover normative paragraphs with spec tests.
  - Drop the `comptime_derives` preview gate; mark this ADR *implemented*.

## Consequences

### Positive

- **Smallest possible mechanism.** One new intrinsic, one new directive, one new sema sub-phase. No new value types, no new syntax, no expansion.
- **Reuses existing primitives.** `@typeInfo`, `comptime_unroll for`, `@field(self, name)`, generic functions, monomorphization — all of these already exist. The derive isn't constructing anything new; it's gluing existing pieces together.
- **Derives migrate from compiler to library.** New derives become standard-library PRs, not language ADRs. The compiler stops being the bottleneck.
- **No hygiene problem.** There's nothing to be hygienic about — the attached method body was authored as an ordinary function, with ordinary scoping.

### Negative

- **`Copy`-shaped derives are not yet expressible.** The MVP can attach methods but not flip type-level flags. A second intrinsic (`@attach_directive` or equivalent) is needed for Copy. Out of scope here; tracked as future work.
- **Attached methods cannot consume `self` field-by-field unless ADR-0036 is relaxed.** A user-written structural Drop derive that does `drop(@field(self, f.name))` runs into the partial-move ban. The mechanism is fine; the ergonomics depend on a separate decision about partial moves inside `fn drop`. Independent ADR.
- **Generic-function-plus-derive feels indirect.** Library authors write two functions instead of one (the implementation, then the registration shim). Mitigated by convention and by stdlib-level helpers; the alternative was a full macro system.
- **Pending-attachments table is a new piece of mutable state in sema.** Small, scoped to the derive sub-phase, drained at sub-phase exit. But it's another invariant to maintain.

### Neutral

- **No ABI change.** Attached methods lower exactly like inline methods; codegen sees no difference.
- **No new comptime value types.** The interpreter's `ConstValue` enum doesn't grow; `@attach_method` is a side-effecting intrinsic that returns unit.
- **Composes with anonymous interfaces.** Attached methods sit in the host type's method list before conformance is checked, so a derive can make T satisfy a named or anonymous interface (ADR-0056/ADR-0057) the same way a hand-written method would. The two ADRs don't share implementation surface — this one writes to method lists, ADR-0057 writes to the interface pool — but they meet at conformance, which is the intended use.

## Open Questions

1. **Where does the "first comptime parameter is `type`" requirement live?** Is it part of the function's declared signature (the user writes `fn impl(comptime T: type, self: T)`), or is it implicit from the attachment intrinsic? *Tentative:* declared explicitly, so reading the function signature tells you it's a derive-compatible body. Implicit binding would be magic.

2. **Should `@attach_method` accept anonymous functions (ADR-0055), or only named top-level functions?** Anonymous functions could let library authors write the implementation inline without a separate top-level name. *Tentative:* named only for MVP. ADR-0055 lambdas have capture semantics that complicate per-T monomorphization; revisit once a real use case appears.

3. **Multi-derive ordering on conflicts.** Two derives attaching different methods is fine. Two attaching the same name errors — but should the error name *both* attachers, or only the second? *Tentative:* multi-span citing both, with the second flagged as the conflicting one.

4. **Should `@attach_method` allow attaching to types other than the directly-derived T?** E.g., a derive on `Foo` attaches a method to `Vec<Foo>`. Powerful, but immediate scoping consequences. *Tentative:* T must be the host type for this ADR; cross-type attachment is future work.

5. **Visibility of attached methods.** If `__structural_drop` is private to a stdlib module, the attached `drop` method on `Buffer` is callable everywhere `Buffer` is — but the underlying function isn't. Is that OK? *Tentative:* yes — methods take their visibility from the type, not from the implementation function. Same as how compiler-internal drop glue today is "called" by user code through scope exit.

## Future Work

- **Type-level directive attachment.** `@attach_directive(T, "copy")` (or similar) to enable `Copy`-shaped derives. Reuses the same phase ordering and `@derive` invocation; the only new piece is the directive-application path in declaration finalization.
- **Concrete stdlib derives.** `Drop`, `Eq`, `Hash`, `Default`, `Clone`, `Ord`. Each is a small PR once the substrate is in.
- **Anonymous-function bodies.** Allow `@attach_method(T, "drop", fn(comptime _: type, self) { ... })` for one-shot inline implementations.
- **Cross-type attachment.** `@attach_method(SomeOtherType, ...)` from a derive — for derives that produce companion types (an `Iterator` derive that attaches `next` to a separate state struct, for instance).
- **Attaching fields, variants, associated types.** Out of scope here and further-future than the directive-attachment work.
- **Macro system.** If, after concrete derives ship, real use cases show up that need RIR construction (e.g. emitting a state-machine struct alongside the methods that drive it), a `quote`-based system can be designed *as an extension* of this mechanism. This ADR doesn't preclude it; it just doesn't pay for it yet.

## References

- [ADR-0025: Compile-Time Execution](0025-comptime.md) — comptime substrate and generic-function monomorphization that this ADR rides on.
- [ADR-0040: Comptime Expansion](0040-comptime-expansion.md) — the AIR-level interpreter that gains one new intrinsic arm.
- [ADR-0042: Comptime Metaprogramming](0042-comptime-metaprogramming.md) — `@typeInfo`, `comptime_unroll for`, `@field(self, name)`. The reading half whose writing half is `@attach_method`.
- [ADR-0053: Unified Inline Methods and Drop Functions](0053-inline-methods-and-drop.md) — the method-list model that `@attach_method` splices into.
- [ADR-0057: Anonymous Interfaces](0057-anonymous-interfaces.md) — precedent for "comptime constructs entities the rest of the compiler treats as native."

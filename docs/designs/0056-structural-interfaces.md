---
id: 0056
title: Structurally Typed Interfaces (Comptime Constraints + Dynamic Dispatch)
status: proposal
tags: [types, generics, comptime, dispatch, polymorphism]
feature-flag: interfaces
created: 2026-04-25
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0056: Structurally Typed Interfaces

## Status

Proposal

## Summary

Add `interface` declarations: named, **structurally typed** sets of method
requirements (Go-style). The same interface name can be used in two distinct
contexts:

1. As a **comptime constraint** — `fn f(comptime T: Drop, t: T)` — forcing
   monomorphization for every concrete `T` proven to conform.
2. As a **runtime type** behind a borrowing parameter — `fn f(inout t: Drop)`
   — passed as a fat pointer `(data, vtable)` and dispatched dynamically.

Conformance is **structural**: any type whose method set covers the interface's
required methods conforms automatically; no `impl Drop for Foo` declaration
exists. The MVP is methods-only (no field requirements). No built-in
interfaces are introduced; `Drop`, `Clone`, etc. are out of scope and become
follow-up ADRs once this primitive is stable.

## Context

### What we already have

Gruel today has two routes to polymorphic code:

1. **Comptime type parameters (ADR-0025)** — `fn id(comptime T: type, x: T)`.
   `T` is unconstrained; the body is re-checked per specialization, so any
   operation used inside the body must work for every concrete `T` actually
   passed. This produces helpful but late-binding errors ("`+` not defined for
   `MyStruct` at instantiation site"), with no way to *declare* the operations
   a generic depends on.
2. **Anonymous functions (ADR-0055)** — lambdas desugar to a struct with a
   `__call` method, making "callable thing" expressible without function-pointer
   or closure types. This solves the higher-order-function problem but only by
   leaning on the same unconstrained monomorphization.

What is missing:

- **Bounded generics**. There is no way to write "`T` must support `drop`",
  short of trusting that the user supplied a type with the right methods and
  letting specialization fail late.
- **Dynamic dispatch**. There is no way to hold "some type that supports
  drop, I don't care which" at runtime. Heterogeneous collections, plug-in
  style code, and interface-erased APIs are not expressible.
- **A foundation for `Drop`**. ADR-0010 (destructors) and ADR-0053
  (inline-methods-and-Drop) currently rely on the compiler recognizing a
  hardcoded `__drop` method on a struct. To make `Drop` a real interface that
  user types can choose to participate in — and that the compiler can call
  generically through `inout` parameters — we need a structural-conformance
  mechanism.

### Why structural (Go-style) and not nominal (Rust traits)

Nominal traits (`impl Trait for Type {}`) require a separate declaration step
per (type, trait) pair, plus orphan rules to keep coherence. Structural
conformance is more in keeping with the rest of Gruel's surface (anonymous
structs, anonymous enums, anonymous functions, structural tuple types) and
matches the user's stated intent. Conformance becomes a property of the
type's existing method set, decided by the compiler at use-site rather than at
declaration site.

### Why the same syntax for comptime and runtime

Conceptually `comptime T: I` and `t: I` are asking the same question: "does
this type expose method set `I`?" The answer is the same; the only difference
is whether the answer is consumed by monomorphization (and so erased before
codegen) or by vtable-based dispatch (and so reified at runtime). Using one
declaration site for both keeps the user's mental model small.

## Decision

### Syntax

```gruel
interface Drop {
    fn drop(self);
}

interface Reader {
    fn read(inout self, buf: ptr mut u8, len: usize) -> usize;
    fn close(self);
}
```

Grammar (added to chapter 6):

```
InterfaceDecl  := "interface" Identifier "{" InterfaceMember* "}"
InterfaceMember := MethodSig
MethodSig      := "fn" Identifier "(" SelfParam ("," ParamList)? ")"
                  ("->" Type)? ";"
SelfParam      := "self" | "inout" "self" | "borrow" "self"
```

Each member is a method *signature* (no body, no associated functions in MVP).
Trailing semicolons are required to disambiguate from method definitions.

### Conformance

A type `T` conforms to interface `I` iff for every required method
`fn name(<self-mode> self, p1: P1, …, pN: PN) -> R` in `I`, there exists a
method on `T` (in any `impl T` block — anonymous structs included) with:

- The same name.
- The same self mode (`self`, `inout self`, or `borrow self`).
- An exactly matching parameter list (count, types, modes — including
  `inout`/`borrow` on non-self params).
- The same return type, modulo the rule that the interface's `Self` (if we
  later introduce it) substitutes for `T`. The MVP does *not* introduce
  `Self`; methods refer to the receiver only via `self`.

Conformance is checked **at the use site**, not at the type's declaration site.

### Two usage modes

#### Mode 1: Comptime constraint

```gruel
fn drop_one(comptime T: Drop, t: T) {
    t.drop();
}
```

Here `Drop` is used in place of `type` as the *bound* of a comptime parameter.
At each call site:

1. The argument bound to `T` is some concrete type `C`.
2. The compiler checks `C` conforms to `Drop`. If not → compile error at the
   call site, citing the missing method(s).
3. Specialization proceeds as today (ADR-0025): `T` is substituted with `C`,
   the body is re-analyzed, and `t.drop()` resolves to `C::drop`.

After monomorphization, no trace of the interface remains in AIR/codegen.

#### Mode 2: Runtime dynamic dispatch

```gruel
fn drop_one(inout t: Drop) {
    t.drop();
}
```

Here `Drop` is used as a *type* in a parameter position. Such parameters must
be passed via a borrowing mode (`inout` or `borrow`) — see "Restrictions"
below. The parameter's ABI is a **fat pointer**:

```
struct InterfaceRef {
    data:   ptr mut/const T_erased,   // mode comes from the borrow
    vtable: ptr const VTable_I,
}
```

A method call `t.drop()`:

1. Resolves `drop` to slot index `k` in `Drop`'s vtable.
2. Lowers to `(t.vtable->slots[k])(t.data, …args)`.

Coercion from a concrete `C` to interface `I` happens implicitly at call sites
where the parameter type is `inout I` (or `borrow I`) and the argument has
type `C`:

1. Compiler checks `C` conforms to `I` (same check as comptime mode).
2. Compiler looks up (or generates) the `<C, I>` vtable as a static.
3. The argument lowers to `{ &mut argument, &VTABLE_C_I }`.

### Restrictions in MVP

| Allowed                                | Not yet                                      |
|----------------------------------------|----------------------------------------------|
| `comptime T: I`                        | Multiple bounds: `comptime T: (I & J)`       |
| `inout t: I`, `borrow t: I`            | By-value `t: I` (would require boxing)       |
| Method requirements                    | Field requirements (`field: T;` in interface) |
| Methods with `self`/`inout`/`borrow`   | `self` in by-value form for runtime mode     |
| Return type `R` with no `Self`         | `Self` keyword in interfaces                 |
| `let r: I = …;` rebinding via borrow   | Returning `I` from a function (`-> I`)       |
| Single-file interface use              | Module visibility / `pub interface`          |

By-value `self` is allowed inside an interface method *signature* — but it can
only be exercised through the comptime path, where the receiver type is known
concretely at codegen time. Calling a by-value-`self` method through a
runtime fat pointer is a compile error; the caller must use the comptime form
or use a `borrow self` / `inout self` method.

### Type system integration

Add `TypeKind::Interface(InterfaceId)` to `gruel-air/src/types.rs`. Add a
parallel `InterfaceDef` (alongside `StructDef`/`EnumDef`) holding the
interface's name, method signatures (in declaration order — that order *is*
the vtable layout), and source span.

Add `Interface(InterfaceId)` to whatever bound representation the comptime
parameter machinery uses for `comptime T: …`. Today the bound on a comptime
type parameter is implicitly `type` (any type). We extend it to allow either
`type` or `Interface(InterfaceId)`.

### Conformance check

A single helper:

```rust
fn check_conforms(
    sema: &Sema,
    candidate: Type,           // typically a Struct or Enum
    interface: InterfaceId,
    use_span: Span,
) -> Result<ConformanceWitness, CompileError>;
```

Returns either a witness (a vector mapping each interface slot to the concrete
method's `(StructId, Spur)`) or an error listing every missing/mismatched
requirement at once (so the user sees the whole gap, not one method at a
time).

The witness is the input both to monomorphization (to resolve method calls on
`T` inside the generic body) and to vtable generation.

### Vtable generation

For each `(concrete type C, interface I)` pair *actually used* at runtime
(i.e. that flows into a coercion), emit a static LLVM constant:

```
@__vtable__C__I = constant { i8*, i8*, … } {
    bitcast (ConcreteSig* @C__m1 to i8*),
    bitcast (ConcreteSig* @C__m2 to i8*),
    …
}
```

Slot order is the interface's method order. Generation is keyed by
`(StructId, InterfaceId)` and deduplicated.

The fat pointer is passed as two pointer-sized values in the C ABI sense (no
struct return / no spilling). On 64-bit targets this is two registers.

### Self consumption and Drop

The user's headline example —

```gruel
interface Drop {
    fn drop(self);
}
```

— uses by-value `self`. Under the MVP that means `Drop`-with-`fn drop(self)`
*can* be used as a comptime constraint (where each specialization has a
concrete receiver type and consumption is fine), but *cannot* be used as a
runtime fat-pointer parameter, because dispatching a consuming method through
a `inout`/`borrow` fat pointer is incoherent.

That is acceptable for this ADR — the goal is to land the interface
machinery, not to land `Drop` itself. A future ADR (or revision of ADR-0010)
can decide whether the canonical `Drop` interface uses `self`, `inout self`,
or both, and how that interacts with the affine system.

### What `interface` is not (in MVP)

- Not nominal: no `impl Drop for Foo`.
- Not inheriting: no `interface Reader: Closer`.
- Not extending: no default-method bodies, no associated constants, no
  associated types.
- Not boxing: no `Box<dyn I>`-equivalent. Owned interface values do not exist.
- Not negative: no `T: !Send`-style anti-bounds.
- Not coherence-checked: structural conformance is intrinsic to the type's
  method set; the orphan-rule problem does not apply.

## Implementation Phases

Each phase ends in a committable, runnable state with the preview flag
`interfaces` enabled. Phases 1–4 can ship sequentially; spec/tests are folded
into each phase but the formal spec chapter lands in Phase 5.

- [ ] **Phase 1: Parsing and RIR**
  - Add `interface` keyword to `gruel-lexer`.
  - Parse `InterfaceDecl` items in `gruel-parser`; reject method bodies and
    associated functions with a clear diagnostic.
  - Add `Item::Interface` to AST and the corresponding RIR `InterfaceDecl`
    instruction. No semantic checking yet beyond duplicate-name detection.
  - Tests: parser-only spec tests verifying the AST/RIR shape and rejection
    of method bodies / associated fns.

- [ ] **Phase 2: AIR representation and conformance check**
  - Add `TypeKind::Interface(InterfaceId)` and `InterfaceDef` in
    `gruel-air/src/types.rs`. Plumb through intern pool / printers /
    `Type::new_interface`.
  - Gather pass: register each `interface` declaration into a
    `HashMap<Spur, InterfaceId>` parallel to `Sema::structs`.
  - Implement `check_conforms(candidate, interface) -> ConformanceWitness`,
    matching against the existing `Sema::methods` table (and anon-struct
    captures) by name + self-mode + param list + return type.
  - Add the new error variants: `InterfaceMethodMissing`,
    `InterfaceMethodSignatureMismatch`, with rich diagnostics that show the
    full required signature next to what the type actually has.
  - Add `PreviewFeature::Interfaces` to `gruel-error`. Gate `interface`
    declarations and any *use* of an interface name in a type/bound position
    on this flag.
  - Tests: positive and negative conformance — missing method, wrong arity,
    wrong return type, wrong self-mode, etc.

- [ ] **Phase 3: Comptime constraint usage (`comptime T: I`)**
  - Extend the comptime parameter bound representation to allow an interface
    in addition to `type`.
  - Parser: `comptime T: SomeInterface` parses as a bounded type param.
  - Specialization (`gruel-air/src/specialize.rs`): when binding `T`, run the
    conformance check against the concrete type. On success, attach the
    witness to the specialization context so that method calls on `T` inside
    the body resolve to the witness's concrete methods. On failure, emit a
    call-site error.
  - Method resolution inside generic bodies: when `T` is interface-bounded,
    `t.method()` typechecks against the interface's signatures (so the body
    is checked once, not per specialization, for any method also listed in
    the interface).
  - Tests: monomorphization with interface bound, conformance failure at the
    call site, methods on `T` resolving correctly per specialization.

- [ ] **Phase 4: Runtime dynamic dispatch**
  - Sema: accept `inout t: I` and `borrow t: I` parameter forms; reject
    by-value `t: I`. Reject by-value-`self` method calls through interface
    typed receivers.
  - AIR: add `InterfaceRef` as the lowered parameter type, plus
    `MethodCallDyn { interface: InterfaceId, slot: u32, recv, args }` for
    dynamically dispatched calls.
  - Codegen (`gruel-codegen-llvm`):
    - Layout: fat pointer = `{ ptr, ptr }`; produce LLVM struct type per
      interface and use it in function signatures.
    - Vtable emission: for each `(StructId, InterfaceId)` pair that flows
      into a coercion site, generate `@__vtable__C__I` once, deduplicated.
    - Coercion lowering: at the call site, materialize
      `{ &(mut|const) arg, &VTABLE_C_I }`.
    - Dynamic dispatch lowering: `MethodCallDyn` becomes
      `load slot k from vtable; call slot(data, …args)`.
  - Tests: dynamic dispatch against several conforming types; mixed comptime
    and runtime usage of the same interface; vtable deduplication (golden
    AIR/asm test).

- [ ] **Phase 5: Specification, traceability, and stabilization**
  - New spec chapter (suggested 4.17 or section 6.5 — pick during writing)
    covering interface declarations, conformance, comptime bounds, and
    runtime dispatch.
  - Cover every normative paragraph with spec tests (`spec = […]`).
  - Update grammar appendix.
  - Once Phase 4 is solid: remove the `Interfaces` `PreviewFeature` variant,
    drop `preview = "interfaces"` from spec tests, mark this ADR
    *implemented*.

## Consequences

### Positive

- **Bounded generics without a trait system.** Comptime type params can now
  carry method-set requirements, so generic bodies typecheck against a real
  contract instead of being re-checked per specialization.
- **Dynamic dispatch becomes possible.** Heterogeneous collections, plug-in
  APIs, and erased callbacks are now expressible (within `inout`/`borrow`).
- **Single mechanism, two modes.** Users learn `interface` once and choose
  monomorphization vs. dispatch by where they put the keyword (`comptime` vs.
  `inout`/`borrow`). No second concept.
- **Foundation for first-class `Drop`.** Lets ADR-0010/0053 stop hardcoding
  `__drop` and instead phrase destruction through an actual interface.
- **No ABI lock-in.** Vtable layout is internal to the compiler; nothing about
  the language commits us to a particular fat-pointer convention if we want
  to revisit it.

### Negative

- **Accidental conformance.** Structural conformance means a type can satisfy
  an interface unintentionally because two methods happen to share a name.
  Mitigation: name interfaces narrowly; consider an opt-in `nominal`
  modifier later if this proves painful.
- **Compile-time cost.** Conformance checks run at every use site of an
  interface bound or coercion; method tables are walked. Likely cheap in
  practice but not free.
- **Vtable code-size cost.** Each `(C, I)` pair in use emits a constant.
  Mitigation: deduplicate aggressively, defer cross-crate concerns to the
  module-system ADR.
- **Sharper edges around `self`.** Allowing `self` (consuming) in interface
  signatures but not in runtime dispatch means the same interface can be
  partially-usable in one mode. We document this clearly; a future ADR can
  unify it (boxing, or a `consuming` calling convention in vtables).
- **Error message surface.** "Type `Foo` does not conform to `Reader`:
  missing method `read(inout self, …)`" needs to be high quality or users
  will hate this feature; this is real diagnostic work, not free.

### Neutral

- **No change to existing concrete-type method resolution.** All current
  `obj.method()` calls keep their current resolution path. The new path only
  fires for interface-typed and interface-bounded receivers.
- **No change to ABI for non-interface code.**
- **Aligned with ADR-0055.** Anonymous functions (callable structs) compose
  with interfaces: an interface `Callable<T, U> { fn __call(self, x: T) -> U;
  }` would let comptime higher-order code take a real bound. Out of scope
  here, but the path exists.

## Open Questions

1. **Which spec section does this live in?**
   Tentative: a new section under chapter 6 (items): `6.5 Interfaces`.
   Alternative: chapter 4 alongside types. Decide while writing the spec.

2. **Should the interface keyword include trailing semicolons on method
   signatures, or use no terminator?**
   The grammar above uses `;`. Alternative: no terminator and rely on
   `fn` being unambiguous. Semicolons are clearer; keep unless they grate.

3. **Do we want a non-bound use of `Interface` as a type alias for `comptime
   T: type where T conforms to I`?**
   I.e., is `fn f(comptime T: Drop, t: T)` the canonical spelling, or could
   we shorten to `fn f(comptime t: Drop)` with `T` implicit?
   *Tentative:* explicit `T` in MVP; revisit if it's a common pattern.

4. **What is the canonical receiver mode for the eventual `Drop`?**
   This ADR deliberately does not answer. Once interfaces land, the `Drop`
   ADR can decide between `fn drop(self)` (consuming, comptime-only),
   `fn drop(inout self)` (works with runtime dispatch), or both via overload.

5. **Should anonymous-struct/anonymous-enum types be able to satisfy
   interfaces?**
   *Tentative:* yes, automatically, via the existing anon-method machinery.
   Worth a dedicated test in Phase 3.

6. **Vtable layout stability across compilation units.**
   Once the module system (ADR-0026) is real, the same interface declared in
   one file may be referenced from another. We need vtable layout to be
   deterministic from the interface's declared method order. This is fine
   inside one compilation unit today; flagged for the module ADR follow-up.

7. **Coercion sites.**
   Implicit `C → I` coercion at call boundaries is the obvious case. Do we
   also allow it in `let` bindings (`let r: borrow Drop = &foo;`) and in
   `return` expressions for interface-typed return values? Phase 4 starts
   with call boundaries only; expand if the ergonomics demand it.

## Future Work

Out of scope for this ADR; each becomes a candidate follow-up:

- **Built-in interfaces.** `Drop`, `Clone`, `Copy`, `Eq`, `Ord`, etc. — each
  is a separate design conversation and tied to existing affine/copy
  machinery. None ship with this ADR.
- **Field requirements in interfaces.** Would require either uniform layout
  (impractical with structural conformance) or per-conformance offset slots
  in the vtable.
- **Multiple interface bounds.** `comptime T: (I & J)`. Conceptually a
  conjunction over conformance witnesses; gated on demand.
- **Default method bodies.** Interfaces with method *implementations* shared
  across all conforming types. Pulls in a lot of trait-system surface.
- **`Self` keyword inside interface signatures.** Useful for things like
  `fn clone(borrow self) -> Self`. Requires a substitution rule both in
  comptime and dynamic-dispatch modes.
- **Owned dynamic dispatch (`Box<dyn I>` analog).** Requires either heap
  boxing or a sized-erasure scheme.
- **Returning interface types from functions.** `fn make() -> impl I` and
  `fn make() -> dyn I` flavors.
- **Cross-module conformance and visibility.** Folds into ADR-0026.
- **Negative bounds / specialization.** Out of scope indefinitely.
- **Anonymous interfaces to allow monomorphization over generics.**

## References

- [ADR-0008: Affine Types and Mutable Value Semantics](0008-affine-types-mvs.md) — receiver semantics, drop invariants
- [ADR-0009: Struct Methods](0009-struct-methods.md) — existing method
  resolution and impl-block plumbing
- [ADR-0010: Destructors](0010-destructors.md) — current `__drop` mechanism
  this ADR enables superseding
- [ADR-0013: Borrowing Modes](0013-borrowing-modes.md) — `inout` / `borrow`
  parameter conventions used by interface-typed parameters
- [ADR-0025: Compile-Time Execution](0025-comptime.md) — comptime parameter
  and monomorphization machinery this ADR extends
- [ADR-0029: Anonymous Struct Methods](0029-anonymous-struct-methods.md) —
  method gathering on synthetic structs
- [ADR-0053: Inline Methods and Drop](0053-inline-methods-and-drop.md) —
  current state of `Drop` handling
- [ADR-0055: Anonymous Functions](0055-anonymous-functions.md) — callable
  structs, the precedent for "shape-based" polymorphism
- Go's interfaces — primary inspiration for structural conformance
- Rust's `dyn Trait` — prior art for fat-pointer dynamic dispatch
- Swift protocols / existential types — prior art for the dual
  static/dynamic usage of one declaration

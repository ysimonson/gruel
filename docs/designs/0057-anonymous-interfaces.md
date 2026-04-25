---
id: 0057
title: Anonymous Interfaces (Comptime-Constructed)
status: implemented
tags: [types, generics, comptime, interfaces]
feature-flag: anon_interfaces
created: 2026-04-25
accepted: 2026-04-25
implemented: 2026-04-25
spec-sections: ["6.6"]
superseded-by:
---

# ADR-0057: Anonymous Interfaces

## Status

Implemented

## Summary

Allow `interface { fn name(self) -> T; ... }` to appear as a `TypeExpr`
inside a comptime function body that returns `type`. Each unique
parameterization produces a distinct, structurally-deduplicated
`InterfaceId`, which slots into the existing ADR-0056 conformance and
vtable machinery without changes. This enables parameterized interfaces
like `fn Sized(comptime T: type) -> type { interface { fn size(self) ->
T; } }` and unblocks designs that need interfaces over types — iterator
shapes, container shapes, comparable-with-key shapes — exactly the same
way anonymous structs and enums (ADR-0029, ADR-0039) unblocked
parameterized data types.

## Context

### What we have

- **Named interfaces (ADR-0056)** — declared at module scope, registered
  as `InterfaceId` in `Sema::interfaces`, used either as a comptime
  constraint (`comptime T: I`) or as a runtime fat-pointer type
  (`borrow t: I` / `inout t: I`). Conformance is structural; vtables are
  emitted per `(StructId, InterfaceId)` pair.
- **Anonymous structs (ADR-0029) and anonymous enums (ADR-0039)** —
  built inside `fn ... -> type` bodies via `struct { ... }` /
  `enum { ... }` type expressions. The comptime interpreter constructs
  them with the enclosing function's comptime params substituted, and
  the type pool deduplicates structurally identical results so two
  call-sites with the same args reuse the same type.
- **Anonymous functions (ADR-0055)** — each lambda site produces a
  fresh callable struct with a `__call` method.

### What's missing

There's no way to parameterize an interface over types or values:

```gruel
// Doesn't parse today:
fn Sized(comptime T: type) -> type {
    interface {
        fn size(self) -> T;
    }
}

fn use_sized(comptime T: type, borrow s: Sized(T)) -> T {
    s.size()
}
```

Users wanting "container of T" or "iterator over T" interfaces are
forced to either:

- Re-declare the interface as a new top-level decl per `T` (duplication
  scales linearly with the parameter space), or
- Drop dynamic dispatch entirely and bounce through `comptime T: type`
  with re-checked bodies (loses the conformance contract).

Since named interfaces, anonymous structs, and the comptime interpreter
are all already implemented, the missing piece is the third corner of
the table: anonymous, comptime-constructed interfaces.

### Why this is small relative to ADR-0056

ADR-0056 had to land:

- A new keyword and parser productions
- A new `Type` variant, `InterfaceDef`, vtable storage, conformance
  algorithm
- AIR / CFG / LLVM lowering for `MakeInterfaceRef` and `MethodCallDyn`
- Per-(struct, interface) vtable globals

This ADR doesn't touch any of that. The conformance check is type-by-
type and doesn't care how the `InterfaceId` came to be. Vtable
emission is keyed on `(StructId, InterfaceId)` so each unique
`Sized(i32)` vs `Sized(i64)` instantiation gets its own vtable
naturally. The only new work is *constructing* an `InterfaceId` from
a comptime expression and *deduplicating* structurally identical
results.

## Decision

### Syntax

Add `interface { ... }` as a new variant of `TypeExpr`, parallel to
`AnonymousStruct` and `AnonymousEnum`:

```gruel
fn Greeter(comptime T: type) -> type {
    interface {
        fn greet(self) -> T;
    }
}
```

Grammar (extending chapter 6.5):

```
interface_type_expr := "interface" "{" { method_sig } "}" ;
method_sig          := "fn" IDENT "(" "self" [ "," params ] ")"
                       [ "->" type ] ";" ;
```

The body is the same `method_sig` form already used for named
interfaces — bodies are not allowed; receiver is `self`; trailing
semicolon required.

### Use sites

The result is a `Type::new_interface(iid)` value, which slots into all
existing interface positions:

```gruel
// Comptime constraint:
fn use_via_comptime(comptime T: type, comptime U: Sized(T), u: U) -> T {
    u.size()
}

// Runtime borrow:
fn use_via_borrow(comptime T: type, borrow s: Sized(T)) -> T {
    s.size()
}
```

### Comptime construction

When the comptime interpreter encounters an `interface { ... }`
expression, it constructs an `InterfaceDef` whose:

- `methods` are the listed signatures, with parameter and return types
  resolved against the current comptime substitution map (so `T` in
  `fn size(self) -> T` resolves to `i32` inside `Sized(i32)`).
- `name` is a synthetic stable string derived from the surrounding
  comptime function and its arguments — e.g. `"__anon_iface_Sized_i32"`
  — used for diagnostics and for the vtable global symbol.

This mirrors how `inject_anon_struct` already builds anonymous
`StructDef`s with substituted field types.

### Structural deduplication

Two `interface { ... }` expressions evaluated at different call sites
must produce the *same* `InterfaceId` if their resolved method
signatures match. Otherwise, `Sized(i32)` would have a different
`InterfaceId` per call site, breaking conformance witnesses and
causing duplicate vtable globals.

The deduplication key is `(method_name, param_types, return_type)` for
each method in declaration order — i.e. the `Vec<InterfaceMethodReq>`
itself. The type pool already has the lock + hash-map machinery for
struct/enum/array dedup; interfaces become a fourth user.

### Bound resolution at use sites

Today `comptime T: SomeInterface` resolves the bound by looking up
`SomeInterface` as a `Spur` in `Sema::interfaces`. For
`comptime T: Sized(i32)` and `borrow s: Sized(i32)`, the bound is an
expression rather than a name. We extend the bound-resolution path to:

1. If the type symbol is a registered interface name → existing
   behavior (`Type::new_interface(named_id)`).
2. Otherwise, attempt to evaluate the type symbol as a comptime
   expression. If it produces a `ConstValue::Type(Type::new_interface(_))`,
   use that.
3. Otherwise, the existing fallback errors apply.

This is symmetric with how `comptime T: type` works today — the bound
is itself a comptime value.

The same path is used for `borrow s: Sized(i32)`-style runtime params:
`resolve_param_type` already accepts interface names; the only change
is to widen "interface name" to "any expression that evaluates to an
interface type" in the comptime context.

### Vtable layout

No change. `(StructId, InterfaceId)` is still the key; each unique
`Sized(i32)`, `Sized(i64)`, `Greeter(bool)` gets its own
`InterfaceId`, hence its own vtable global. Conforming types whose
methods happen to satisfy multiple parameterizations get a vtable per
parameterization.

### Restrictions in MVP

| Allowed                                 | Not yet                                 |
|-----------------------------------------|-----------------------------------------|
| Anon iface in `fn ... -> type` body     | Anon iface as inline parameter type     |
| Comptime parameterization over `type`   | Anon iface with bodied methods          |
| Comptime parameterization over values   | `Self` keyword in method signatures     |
| Methods over substituted types          | Method-level comptime params (yet)      |
| Structural dedup across instantiations  | Cross-module shared vtable interning    |

The "inline parameter type" exclusion means `fn foo(borrow t: interface
{ fn read(self) -> i32 })` is *not* allowed. The motivating use case
for anonymous interfaces is parameterization; ad-hoc inline structural
typing is a separate decision (Go-style) that we can revisit if there's
demand.

`Self` in interface method signatures is deferred for the same reason
ADR-0056 deferred it — the dyn-dispatch object-safety rules remain an
open design question. Anonymous interfaces don't make this easier or
harder.

## Implementation Phases

Each phase is independently committable. Phases share a single preview
flag (`anon_interfaces`); stable when the last phase lands.

- [x] **Phase 1: Parser + AST + RIR for `interface { ... }`**
  - Add `TypeExpr::AnonymousInterface { methods: Vec<MethodSig>, span }`
    parallel to `AnonymousStruct`/`AnonymousEnum`.
  - Extend `chumsky_parser` to recognize `interface` in type-expression
    position (currently it's only parsed as a top-level item).
  - Add `InstData::AnonInterfaceType` to RIR so it can flow through the
    comptime interpreter.
  - Tests (preview, allowed-to-fail-codegen): `interface { ... }` parses
    inside a `fn -> type` body without crashing the frontend.

- [x] **Phase 2: Comptime construction of `InterfaceDef`**
  - In sema's comptime evaluator, add an arm for `AnonInterfaceType`
    that builds an `InterfaceDef` with method-sig types resolved under
    the current substitution map.
  - Synthesize a stable name like `__anon_iface_<n>` (the same scheme
    anon structs use, but with their own counter so dump output is
    distinguishable).
  - Hand the `InterfaceDef` to the type pool's intern path; on
    cache-hit, return the existing `InterfaceId`; on miss, register a
    new one.
  - Tests: identical anon-iface expressions evaluated twice produce the
    same `InterfaceId`.

- [x] **Phase 3: Bound resolution from expressions**
  - Extend `resolve_param_type` and the comptime-bound machinery so
    `comptime T: <expr>` and `borrow t: <expr>` accept any expression
    whose comptime value is `Type::new_interface(_)`.
  - Wire through the existing `try_evaluate_const` /
    `resolve_type_for_comptime` pipelines.
  - Tests: `Sized(i32)` works as both a comptime bound and a runtime
    param type; `Sized(i32)` and `Sized(i64)` are distinct interfaces;
    method calls on receivers of type `Sized(i32)` typecheck against
    the substituted signature (`-> i32`, not `-> T`).

- [x] **Phase 4: Vtable emission for parameterized pairs**
  - Vtable globals are already keyed on `(StructId, InterfaceId)`, so
    no new mechanism is needed — but verify dedup actually fires across
    `Sized(i32)` instantiations from multiple call sites.
  - Add a spec test that calls the same parameterized borrow from two
    sites and asserts (via exit code) the dispatch lands in the right
    methods.
  - End-to-end runnable: a parameterized interface program compiles and
    runs, dispatching dynamically through a vtable named after the
    instantiated interface.

- [x] **Phase 5: Spec, traceability, stabilization**
  - New paragraphs under chapter 6.5 covering anon-iface syntax,
    comptime construction, structural dedup, and bound-resolution
    semantics.
  - Cover every normative paragraph with spec tests.
  - Remove the `AnonInterfaces` preview flag, drop
    `preview = "anon_interfaces"` from spec tests, mark this ADR
    *implemented*.

## Consequences

### Positive

- **Parameterized interfaces**, completing the symmetry started by
  ADR-0029 (anon structs) and ADR-0039 (anon enums). The pattern
  `fn TypeCtor(comptime T: type) -> type { struct/enum/interface { ... } }`
  becomes uniform.
- **No new runtime concept**. Vtables, fat pointers, conformance — all
  unchanged. The new code is small (~700 lines plus tests) and
  localized to the comptime interpreter and the bound-resolution path.
- **Foundation for stdlib interface design**. Things like
  `Iterator(T)`, `IntoIter(T)`, `Comparable(T)` become expressible
  directly without re-declaration per element type.

### Negative

- **Vtable proliferation**. One vtable per `(StructId, InterfaceId)`
  pair, per parameterization. For a conforming type used through three
  parameterizations of the same shape, that's three vtables. Same
  trade-off as monomorphized struct generics; quantifiable but not
  pathological.
- **Diagnostic surface**. "Type `Foo` does not conform to interface
  `__anon_iface_42`" is unhelpful — diagnostics need to render the
  parameterization in source-shape (`Sized(i32)` rather than the
  synthetic name). Mitigated by reusing the anon-struct rendering
  helper, which already does this for struct types.
- **Adds another comptime construction path**. The comptime evaluator
  grows another arm; bugs in the substitution map propagate to
  interface signatures the same way they do for struct fields. Worth
  testing the substitution thoroughly.

### Neutral

- **No ABI change**. Anon interfaces use the same fat-pointer layout
  and vtable scheme as named ones.
- **No interaction with `Self`**. Both named and anonymous interfaces
  defer `Self` until the object-safety design question is resolved.
- **Method-level comptime params.** Generic methods *inside* an anon
  interface (e.g. `fn map(self, comptime U: type, f: ...) -> ...`) are
  out of scope here — the original ADR-0056 deferred them and this ADR
  inherits that posture.

## Open Questions

1. **Vtable dedup across modules.** Once cross-module compilation lands
   (ADR-0026), is the type pool shared so two modules using `Sized(i32)`
   share one vtable, or does each module emit its own?
   *Tentative:* per-module emission with linker-level merge via
   weak-linkage; revisit when the module system is ready to consume
   shared interface IDs.

2. **Inline anonymous interfaces at param positions.** `fn foo(borrow t:
   interface { fn read(self) -> i32 })` is conceivable but doesn't fit
   the comptime construction model. Skip until there's demand?
   *Tentative:* skip. Users can wrap with a one-line `fn` if they want
   the inline shape.

3. **Method-name collisions with multiple parameterizations.** If
   `Sized(i32)` and `Sized(i64)` both expect `fn size(self) -> T`, can
   one type conform to both? Yes, by having both `fn size(self) -> i32`
   and... wait, you can't have two `size` methods with different return
   types. Worth a diagnostic that names the collision precisely.

4. **Anonymous-interface diagnostics.** Should the synthetic name leak
   into error messages, or should we always render the source-level
   parameterization? *Tentative:* render the source form when possible
   (existing anon-struct path); fall back to the synthetic name when no
   source form is recoverable.

## Future Work

- **`Self` in interface signatures.** Tracked separately as part of the
  larger interface-extensions design.
- **Inline interfaces at parameter positions.** Re-evaluate after this
  ADR's parameterized form has shipped and seen real use.
- **Method-level comptime generics on anon interfaces.** Currently no
  interface (named or anon) supports method-level comptime params; when
  that lands for named interfaces, anon interfaces should pick it up
  for free.
- **Interface inheritance / extension.** `interface Bigger extends
  Smaller { ... }` is its own design. Anon interfaces don't change the
  decision but make a parameterized version easy if/when it's wanted.

## References

- [ADR-0025: Compile-Time Execution](0025-comptime.md) — comptime
  interpreter and the `fn ... -> type` machinery this ADR extends.
- [ADR-0029: Anonymous Struct Methods](0029-anonymous-struct-methods.md)
  — the architectural precedent: comptime-built nominal types with
  structural dedup.
- [ADR-0039: Anonymous Enum Methods](0039-anonymous-enum-types.md) —
  symmetric precedent for enums.
- [ADR-0055: Anonymous Functions](0055-anonymous-functions.md) — the
  third "anonymous comptime type" (callable structs).
- [ADR-0056: Structurally Typed Interfaces](0056-structural-interfaces.md)
  — the named-interface foundation this ADR extends.

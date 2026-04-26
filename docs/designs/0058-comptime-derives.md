---
id: 0058
title: User-Defined Derives via `derive` Items
status: proposal
tags: [comptime, metaprogramming, types, derives]
feature-flag: comptime_derives
created: 2026-04-26
accepted:
implemented:
spec-sections: ["4.14"]
superseded-by:
---

# ADR-0058: User-Defined Derives via `derive` Items

## Status

Proposal

## Summary

Add a single new top-level item kind, `derive Name { <method-decls> }`, whose body is a list of inline method declarations referring to the target type as `Self`. A `@derive(Name)` directive on a struct or enum invokes the derive at a new sema sub-phase, splicing each method into the host type's method list with `Self` bound to the host. The mechanism is expressive enough to write `Drop`, `Eq`, `Hash`, `Default`, and `Clone` in user code; **no concrete derives ship in this ADR** — the deliverable is the substrate. There is no `quote`, no syntactic capture, no macro expansion, no statement-level extension form, no new comptime value type, and no explicit type parameter on the derive itself.

## Context

### Where comptime stands

After ADR-0042, comptime can read type structure (`@type_info`), iterate (`comptime_unroll for`), access fields by comptime-known name (`@field(self, name)`), and emit diagnostics. After ADR-0057 it can construct anonymous types. The interpreter produces `ConstValue`s. None of this lets a user say "make this function a method of a type."

### Designs considered and rejected

Three intermediate designs informed this one:

- **`@attach_method(T, name, F)` intrinsic.** Forces a function reference to flow through the comptime evaluator as a value. `ConstValue` has no `Function` variant; either every match grows an arm or the third argument gets special-cased syntactically. The "method body" also has to live as a separate top-level generic function with an artificial `comptime _: type` parameter — an indirection unmotivated by anything except the intrinsic's call shape.
- **`fn Drop(comptime T: type) { extend T { ... } }`.** Cleaner than the intrinsic — function bodies are captured directly by the parser — but requires two new legality rules ("comptime-only," "derive-only path") and a runtime context guard inside the comptime evaluator.
- **`derive Drop(comptime T: type) { ... }`.** A new item kind whose body is method declarations. Better still — body grammar shrinks, legality rules disappear — but every derive forces the author to invent a type-parameter name (`T`, `U`, ...) when there's only ever one such parameter and `Self` is already the language's idiomatic "the type this method belongs to."

The unifying observation: a derive's only job is to emit methods on a target type. There's exactly one type involved. `Self` already names "the receiver's type" inside method bodies (ADR-0053), which is the same concept here. Removing the parameter list yields the smallest surface that does the job.

### What this ADR proposes

```gruel
derive Drop {
    fn drop(self) {
        comptime_unroll for f in @type_info(Self).fields {
            drop(@field(self, f.name));
        }
    }
}

@derive(Drop)
struct Buffer { name: String, capacity: i32 }
```

`derive` is a new top-level item kind. Its body is the same method-declaration grammar already used inside type bodies (ADR-0053). `Self` refers to the type the derive is being attached to — a free type variable at derive-definition time, bound to the host type when `@derive(...)` causes the derive to be expanded.

When the compiler encounters `@derive(Drop)` on `Buffer`, it walks the methods inside `Drop`'s body and inserts each one into `Buffer`'s method list with `Self = Buffer`. After the sub-phase exits, attached methods are indistinguishable from hand-written inline methods.

## Decision

### Syntax

```
derive_item := "derive" IDENT "{" { method_decl } "}" ;
method_decl          := "fn" IDENT "(" param_list ")" [ "->" type ] block ;
```

`derive` takes no parameter list — there is exactly one implicit free type variable, `Self`, in scope inside every method body. Method-decl grammar is the same as inline methods inside a struct or enum body.

### `Self` inside a derive

`Self` resolves to the host type at derive-expansion time. At derive-definition time it is a free type variable for type-checking purposes. References to `Self` in method bodies — `@type_info(Self)`, `let x: Self = ...`, `Self::associated_fn()` — work exactly as they do inside a struct or enum body, with the receiver type unknown until expansion. Field access on `self` (lowercase, the receiver) requires `@field(self, comptime_name)` because `Self`'s structure isn't statically known at the derive site.

### `@derive(D)` directive

```
@derive(D)
struct ... { ... }
```

Applied to a struct or enum, `@derive(D)` invokes the derive `D` against the host type during a new sema sub-phase (see [Phase ordering](#phase-ordering)). `D` is resolved against the surrounding scope; it must name a `derive` item. Resolving to anything else (a regular `fn`, a struct, an unknown name) errors with "expected a derive, found ...".

Multiple `@derive(...)` directives on one type run in source order. Each adds its methods to the host type's method list. Conflicts (two derives, or one derive and a hand-written inline method, claiming the same method name) are errors with multi-span diagnostics citing both attachers.

### Method body type-checking

Methods declared inside a `derive` are type-checked **once at derive-definition time** with `Self` as a free type variable. Field access on `self` requires `@field(self, comptime_name)`; direct projection (`self.x`) is rejected because `Self`'s structure isn't known.

This pre-checking catches common errors at the derive author's site. A derive that misuses `Self` produces one diagnostic when the derive item is analyzed; users who write `@derive(Broken)` see the original error plus a single secondary span at their `@derive` directive.

### Method splicing

When `@derive(D)` on `host_type` is processed at the sub-phase, the compiler:

1. For each method in `D`'s body, constructs a fresh `MethodInfo` with `Self` bound to `host_type`. Method bodies are not copied or rewritten; the existing generic-method monomorphization machinery (ADR-0025) handles substitution at first call.
2. Inserts each into `Sema::methods[(host_struct_id, method_name)]` (or `enum_methods` for enums). Conflicts are detected here.
3. Records provenance: which `derive` item each attached method came from, and which `@derive(D)` directive caused the attachment, both as spans for diagnostics.

After step 3, the host type's method list looks the same to every downstream pass (HM, drop elaboration, codegen, dispatch) as if the methods had been written inline in the type body.

### Phase ordering

For named struct/enum declarations, `derive` invocation runs in a new sub-phase between field-type resolution and destructor / Copy validation:

```
parse → RIR
  → declaration gathering (names, fields, raw inline methods, derive items)
  → field-type resolution
  → ★ derive expansion ★            ← new sub-phase (named types)
  → destructor / Copy validation     (sees attached methods)
  → HM constraint generation         (type-checks attached method bodies under host Self)
  → ...
```

The sub-phase iterates named types with `@derive` directives in source order, splicing each derive's methods. No comptime function call is required — splicing is a direct compiler operation since the methods are already in the derive item's RIR.

### Anonymous struct/enum hosts

Anonymous structs and enums (ADR-0029, ADR-0039) are constructed *during* sema by the comptime interpreter, on demand when their parameterization is encountered. They don't exist at the named-type sub-phase, so they need a second splice site.

Surface syntax: `@derive(...)` sits on the anonymous `struct` / `enum` expression, exactly as on a named declaration:

```gruel
fn FixedBuffer(comptime N: i32) -> type {
    @derive(Drop)
    struct {
        name: String,
        data: [i32; N],
    }
}
```

When the comptime interpreter constructs an anonymous `StructDef` / `EnumDef`, it processes the source expression's `@derive` directives *before* handing the def to the type pool: each derive is resolved, its methods spliced into the freshly-built type's method list, and only then is the def registered. From the type pool's perspective the methods are part of the type's identity from registration onward, exactly as if they had been written inline.

Per-instantiation behavior:

- `FixedBuffer(8)` produces a fresh `StructId` with `Drop`'s methods spliced under `Self = FixedBuffer(8)`. Monomorphization sees `data: [i32; 8]` in `@type_info(Self).fields`.
- `FixedBuffer(16)` produces a distinct `StructId`, with a separate splice and methods monomorphized over `data: [i32; 16]`.
- A second call site invoking `FixedBuffer(8)` hits the type pool's structural-dedup path, finds the existing `StructId` with methods already in place, and does not re-splice.

The same single splicing routine is shared between the two sites — the difference is *when* it runs, not *what* it does.

What anonymous-host derives do **not** see:

- The captured comptime parameters (e.g. `N`). Derives reason structurally over `@type_info(Self).fields`, which reports the substituted field types. If a derive ever needs the captured comptime value itself, that's a separate feature paralleling ADR-0057's `anon_struct_captured_values` table.
- The source-level type expression. The host is the resolved anonymous type, not the `fn FixedBuffer(...) -> type` that produced it.

### Resolution at use sites

When user code calls `instance.drop()`, method lookup finds the entry registered by the derive sub-phase and dispatches to its body. Monomorphization with `Self = Buffer` happens on first call, identical to how generic-method dispatch already works. The only difference is that the method name was bound at derive-expansion time rather than at parse time.

Diagnostics inside an attached method body cite the original method's span inside the `derive` item, with a secondary span at the `@derive(D)` directive that caused the attachment. Users see "error in `Drop::drop` (attached to `Buffer` by `@derive(Drop)`)."

### What this MVP can't do (deliberately)

- **Pre-emission validation.** A `derive` body is method declarations only — no place for "if `Self`'s fields aren't Copy, abort." Validation must live inside method bodies (firing at monomorphization) or wait for a follow-up `where` clause.
- **`Copy`-shaped derives.** `Copy` is a type-level flag, not a method. Setting it requires a separate mechanism, out of scope here.
- **Cross-type attachment.** A `derive` only emits methods on `Self`. No way to also emit on a different type from the same derive.
- **Top-level item attachment.** `derive` only emits methods on `Self`, not free functions, helper types, or constants.

## Implementation Phases

Phases share the `comptime_derives` preview flag; stable when phase 6 lands.

- [x] **Phase 1: Parse `derive` items**
  - Lexer: add `derive` as a reserved keyword.
  - Parser: produce `RirItem::Derive { name: Spur, methods: ... }` parallel to `RirItem::Function` / `RirItem::Struct`.
  - Method-decl bodies parse via the existing inline-method grammar.
  - Tests (preview, allowed-to-fail): a `derive` item parses without crashing the frontend.

- [x] **Phase 2: Sema validation of derive bodies**
  - Register each derive in a new `Sema::derives: HashMap<Spur, DeriveInfo>` table during declaration gathering.
  - Type-check each method body with `Self` as a free type variable. Field access on `self` requires `@field`; direct projection errors with a clean diagnostic.
  - Reject malformed derive bodies (anything other than method declarations).
  - Tests: well-formed derives type-check; ill-formed ones (direct field access on `self`, non-method items in body) error cleanly.

- [x] **Phase 3: `@derive(D)` directive parsing and resolution**
  - Extend the directive parser for `@derive(IDENT)`.
  - Resolve `D` against `Sema::derives`; record the binding `(host_type, derive_id)` for the sub-phase. Error if `D` doesn't name a `derive` item.
  - Tests: parsing accepts the directive; resolution errors on a non-derive target.

- [x] **Phase 4: Derive expansion (named and anonymous)**

  Both call sites are implemented end-to-end. Named hosts (struct
  declarations carrying `@derive(...)`) splice during a sub-phase
  between field-type resolution and destructor / Copy validation;
  each binding's methods are inserted into `Sema::methods` (or
  `Sema::enum_methods`) with `Self` bound to the host. Anonymous
  hosts (`@derive(...)` on `struct { ... }` / `enum { ... }`
  expressions inside comptime functions) splice from inside the
  comptime evaluator's anonymous-type construction path. Each fresh
  `StructId` per parameterization gets its own splice; structural
  dedup short-circuits identical parameterizations so methods aren't
  double-spliced. The splicing routine itself is shared between both
  call sites.
  - Factor splicing into a single routine `splice_derive_methods(derive_id, host_type) -> CompileResult<()>` that walks the derive's method list and inserts each into `Sema::methods` / `Sema::enum_methods` with `Self` bound to `host_type` and provenance recorded.
  - **Named-type call site.** Insert a new sub-phase after field-type resolution, before destructor/Copy validation. For each `(host_type, derive_id)` binding from phase 3, call the splicing routine.
  - **Anonymous-type call site.** Hook the comptime interpreter's anonymous-`StructDef`/`EnumDef` construction path: after the def is built but before it's interned in the type pool, walk the source expression's `@derive` directives and call the splicing routine. Structural dedup short-circuits on cache hit so methods aren't re-spliced.
  - Reject duplicates (cross-derive, derive-vs-inline) at insertion with multi-span diagnostics. Same routine on both call sites — diagnostics shape is identical.
  - Tests: end-to-end one-method derive on a named struct lands as a callable method; same on an anonymous struct produced from a comptime function; two parameterizations of the same anonymous type each get their own monomorphized methods; structural dedup of identical parameterizations does not double-splice; an empty derive is a clean no-op; two derives attaching the same name fail with a clear multi-span error on both sites.

- [x] **Phase 5: Method dispatch and diagnostics**
  - Verify attached methods are reachable through normal method-call resolution and that monomorphization handles them like any other generic method.
  - Plumb provenance through error reporting: type errors inside an attached body cite the method's span inside the derive item and the `@derive(...)` directive span.
  - Tests: a runtime-end-to-end derive test (`@derive(Drop)` with the example above runs the cleanup); diagnostics tests for attachment provenance.

- [x] **Phase 6: Spec + traceability (stabilization deferred with anon hosts)**
  - Spec section 4.14 (comptime) gained paragraphs `4.14:100..107`
    covering syntax, the preview gate, the splicing semantics, and the
    legality rules around name collisions and `self.field` projection.
  - All normative paragraphs are covered by tests; traceability stays
    at 100% normative coverage.
  - The `comptime_derives` preview gate **stays in place** until
    anonymous-host expansion lands (see phase 4 follow-up). Marking
    this ADR *implemented* and dropping the gate happens in that
    follow-up — exposing only the named-host case under a stable
    surface would surprise users who reach for the anonymous-host form
    documented in this ADR.

## Consequences

### Positive

- **Smallest possible surface.** One new keyword, one new item kind, one new directive resolution path. No new statement form, no new comptime value type, no new intrinsic, no new evaluation rule, no parameter list to teach.
- **`Self` is already familiar.** Users who know `Self` from struct/enum method bodies (ADR-0053) read `derive` bodies with no new convention to learn. The derive case is just "the receiver type is unknown until expansion."
- **Body grammar is reused.** The method-declaration grammar inside a `derive` is identical to inline methods in a struct (ADR-0053).
- **Errors land at the derive author's site.** Method bodies are type-checked once at derive-definition time; bugs in `Drop` are reported when `Drop` is analyzed, not at every `@derive(Drop)` use.
- **Composes with anonymous interfaces.** Attached methods sit in the host type's method list before conformance is checked, so a derive can make `Self` satisfy a named or anonymous interface (ADR-0056/ADR-0057) the same way a hand-written method would.
- **Migrates derives from compiler to library.** New derives become standard-library PRs.

### Negative

- **No pre-emission validation in MVP.** A derive can't say "if `Self`'s fields aren't Copy, abort." Validation must live in method bodies (late) or wait for a follow-up `where` clause. Trade-off accepted: every viable concrete MVP-target derive (`Drop`, `Eq`, `Hash`, `Default`, `Clone`) is pure method emission.
- **`Copy`-shaped derives are not yet expressible.** Type-level flag setting needs a separate mechanism. Out of scope here, future work.
- **Attached methods cannot consume `self` field-by-field unless ADR-0036 is relaxed.** A user-written structural `Drop` derive that does `drop(@field(self, f.name))` runs into the partial-move ban. The mechanism is fine; ergonomics depend on a separate decision about partial moves inside `fn drop`. Independent ADR.
- **One more reserved keyword.** `derive` joins `fn`, `struct`, `enum`, `interface`. Cost is small but non-zero, and `derive` may collide with existing user code; the rename can be revisited if collisions prove disruptive.

### Neutral

- **No ABI change.** Attached methods lower exactly like inline methods.
- **No new comptime value types.** The interpreter is unchanged; derive expansion is a direct compiler operation, not a comptime evaluation.
- **Method-list mutability.** `Sema::methods` is already mutable post-declaration-gathering; the new sub-phase inserts into it the same way method registration always has.

## Open Questions

1. **`derive` body restrictions.** Should the method-decl grammar inside a derive support exactly the same forms as inside a struct body, including associated functions (no `self`)? *Tentative:* yes; an associated function on `Self` from a derive is a useful capability and adds no new mechanism.

2. **`@derive(D)` resolution scope.** If a `derive` is defined in another module, how is it referenced? *Tentative:* defer to the module system (ADR-0026); for this ADR `D` must be in the current file's flat namespace, matching the rest of the language pre-modules.

3. **Conflict diagnostics.** When two derives both attach `drop`, the error should cite both. Should it cite both `derive` items, both `@derive(...)` directives, or all four? *Tentative:* multi-span with the second `@derive` as primary, secondary spans on the conflicting method declarations and the first `@derive`.

4. **Visibility of attached methods.** If `Drop` is private to a stdlib module, the attached `drop` method on `Buffer` is callable everywhere `Buffer` is — but the `Drop` derive isn't. *Tentative:* methods take their visibility from the host type, not from the derive. Same as anonymous-interface vtable semantics.

5. **`Self` outside method bodies inside a derive.** Does `derive` allow associated constants or type aliases that mention `Self`? *Tentative:* deferred. MVP is method-decls only; revisit when a use case needs more.

6. **Access to captured comptime parameters of an anonymous host.** A derive applied to `FixedBuffer(N)` cannot see `N` directly — only the resolved field types via `@type_info(Self).fields`. Sufficient for `Drop`/`Eq`/`Hash`-shaped derives; insufficient for any derive whose behavior depends on the captured value itself. *Tentative:* deferred. If a real use case appears, expose captured values through a `@type_info` extension rather than a new derive-side intrinsic.

## Future Work

- **Type-level state from derives.** A mechanism for setting type-level flags (`@copy`, `linear`, etc.) so `Copy` itself becomes a derive.
- **Concrete stdlib derives.** `Drop`, `Eq`, `Hash`, `Default`, `Clone`, `Ord`. Each is a small PR once the substrate ships.
- **Macro system.** If real use cases later need RIR construction (emitting state-machine structs alongside their methods), a `quote`-based system can be designed *as an extension* of this mechanism.

## References

- [ADR-0025: Compile-Time Execution](0025-comptime.md) — comptime substrate and generic-function monomorphization that this ADR rides on.
- [ADR-0042: Comptime Metaprogramming](0042-comptime-metaprogramming.md) — `@type_info`, `comptime_unroll for`, `@field(self, name)`. The reading half whose writing half is `derive`.
- [ADR-0053: Unified Inline Methods and Drop Functions](0053-inline-methods-and-drop.md) — the method-declaration grammar reused for derive bodies, the method-list model splicing inserts into, and the existing `Self` convention this ADR generalizes.
- [ADR-0057: Anonymous Interfaces](0057-anonymous-interfaces.md) — precedent for "comptime constructs entities the rest of the compiler treats as native."

---
id: 0076
title: Pervasive `Self` and Sole-Form References
status: proposal
tags: [types, syntax, ownership, borrowing, self, references]
feature-flag:
created: 2026-05-03
accepted:
implemented:
spec-sections: ["6.1", "6.5"]
superseded-by:
---

# ADR-0076: Pervasive `Self` and Sole-Form References

## Status

Proposal

## Summary

Finish ADR-0062 by collapsing the surface syntax for borrowed parameters
and method receivers to a single form: the type constructors `Ref(T)` and
`MutRef(T)`. Make `Self` a first-class type in any position inside a
struct/enum/derive/drop body so that `Ref(Self)`, `MutRef(Self)`,
`Option(Self)`, etc. all work uniformly. Specify and tighten bare-name
assignment as the sole "write through a `MutRef`" form — `r = v` writes
into the pointee — and delete every legacy alternative
(`borrow`/`inout` keywords, `&self` / `&mut self` receiver sugar, the
ad-hoc `Self`-string-comparison branches sprinkled through sema).

## Context

ADR-0062 introduced `Ref(T)` / `MutRef(T)` and `&x` / `&mut x` as the new
surface form for borrows. Phase 8 of that ADR landed the type system,
borrow checker port, codegen, and the through-read / through-write
behaviour for projections (`p.x`, `arr[i]`), but stopped short of full
removal:

1. **Two parallel surface forms still exist.** `borrow x: T` and
   `x: Ref(T)` both parse; `&self`, `&mut self`, `borrow self`, and
   `inout self` all desugar to `SelfMode::Borrow` / `SelfMode::Inout`.
   The lexer keeps `borrow` / `inout` keywords; the AST keeps
   `SelfMode::Borrow`/`Inout` and `ParamMode::Borrow`/`Inout`.
2. **Bare-name assignment through a `MutRef` is implemented but not
   specified as the language rule.** Today it works because parameter
   types `Ref(T)`/`MutRef(T)` are normalised to legacy
   `(T, Borrow)`/`(T, Inout)` pairs in `analyze_function`
   (`crates/gruel-air/src/sema/analysis.rs:1737`); through-write rides
   on the legacy mode machinery and the spec wording in
   `06-items/01-functions.md` still talks in keyword terms.
3. **Interface-typed parameters cannot use the new form.**
   `resolve_param_type` only accepts an interface name when the
   declared mode is the legacy `Borrow`/`Inout`
   (`crates/gruel-air/src/sema/typeck.rs:152`). `t: Ref(SomeIface)` and
   `t: MutRef(SomeIface)` do not compose with the interface ABI, which
   is the second of the "narrower language gaps" Phase 8 calls out.
4. **`Self` resolution is ad-hoc.** Multiple code paths in
   `sema/analysis.rs` (e.g. lines 9849, 9858, 9939, 9948, 9652, 9668)
   short-circuit on the literal string `"Self"` before falling back to
   `resolve_type`. This works in the contexts where a branch was added
   but breaks down inside type constructors — `Vec(Self)`, `Ref(Self)`,
   `Option(Self)`, `(Self, Self)` — because the `Self` token never
   reaches a substitution site once it is wrapped in a `TypeCall` or
   tuple. The spec already promises in `06-items/05-interfaces.md:180`
   that `Self` substitutes "every occurrence", but the implementation
   only honours the leaf-level cases.

The result is the worst of both worlds: ADR-0062 is "implemented" yet
the parser and tests carry the old grammar, the spec carries the old
prose, and `Self` is partially broken. This ADR commits to the end
state: one surface form, one resolution path, one place that lowers
references for the rest of the compiler.

## Decision

### Sole surface form

After this ADR, the only legal way to express a non-owning parameter or
receiver is to write a type:

```gruel
fn read(r: Ref(BigData)) -> i32 { ... }
fn mutate(b: MutRef(Buf)) { ... }

struct Counter {
    n: i32,

    fn get(self: Ref(Self)) -> i32 { self.n }
    fn set(self: MutRef(Self), v: i32) { self.n = v }
    fn consume(self: Self) -> i32 { self.n }
}
```

The following are removed entirely:

- `borrow x: T` and `inout x: T` parameter syntax.
- `borrow expr` and `inout expr` argument syntax.
- `&self`, `&mut self`, `borrow self`, `inout self` receiver sugar.
- The `borrow` and `inout` keywords (lexer drops the tokens; both
  identifiers become available again).
- `RirParamMode::Borrow`, `RirParamMode::Inout`, `RirArgMode::Borrow`,
  `RirArgMode::Inout`, `SelfMode::Borrow`, `SelfMode::Inout`,
  `ParamMode::Borrow`, `ParamMode::Inout`. Receiver/parameter modes
  collapse to `Normal` / `Comptime`; ref-ness is encoded in the type
  itself.

`SelfParam` becomes purely a binding-shape carrier (the literal `self`
identifier with an optional `: T` annotation, defaulting to `Self`); it
no longer carries a separate mode field.

### Pervasive `Self`

`Self` becomes a type that participates in normal type resolution. Two
mechanical changes:

1. `resolve_type` (and `resolve_param_type`) consult a
   `current_self: Option<Type>` field on `Sema`. When the symbol
   `"Self"` is encountered at any depth inside a `TypeExpr` —
   leaf, inside `Ref(...)` / `MutRef(...)` / `Vec(...)` /
   `Option(...)` / tuple element / array element — the resolver
   substitutes the in-scope concrete type.
2. Every place that today does `if type_str == "Self" { struct_type }
   else { resolve_type(...) }` is deleted; the resolver handles it.
   Self-substitution happens in **one** place.

Scopes where `current_self` is set:

| Construct                                     | `current_self`                     |
|-----------------------------------------------|------------------------------------|
| Method defined inside a `struct` body         | the struct type                    |
| Method defined inside an `enum` body          | the enum type                      |
| `derive D { ... }` body                       | the host type at splice time       |
| `interface I { ... }` body                    | the abstract `IfaceTy::SelfType`   |
| Anonymous-fn methods on a comptime-built type | the built type                     |
| Outside any of the above                      | `None` — `Self` is an unknown type |

The destructor cases (the inline `fn drop(self)` method form from
ADR-0053, and the still-supported top-level `drop fn TypeName(self)`
form) inherit `current_self` from the host type; no new context kind
is needed for them.

Using `Self` outside a context that defines it remains an error, with
the existing "Self is reserved inside an interface or struct/enum
body" diagnostic surfaced consistently.

### Bare-name write-through (the only deref-write)

A binding whose declared type is `MutRef(T)` (parameter or local) treats
`name = expr` as **write-through**: `expr` is evaluated to a `T` and
stored at the pointee. This rule is symmetric with the already-working
read forms (`r * 2`, `r.field`, `arr[i]`) and supersedes the deferred
"bare deref operator" mentioned in ADR-0062 Phase 8 — there is no `*r`
form and none is added.

Concrete rules (normative, will land in spec section 6.1):

- For any binding `r` of type `MutRef(T)`, the assignment `r = e` is
  equivalent in dynamic semantics to a store of `e` (after coercion to
  `T`) at the place referenced by `r`. The binding `r` itself is never
  rebound.
- For any binding `r` of type `Ref(T)`, `r = e` is a compile-time
  error (`MutateBorrowedValue`).
- Place projections through `r` continue to work: `r.field = e`,
  `r[i] = e`. These were already specified by ADR-0062.
- The `e` operand is evaluated before the address of the pointee is
  taken (matches existing `Inout` codegen — no change).
- Refs remain scope-bound. A `let r: MutRef(T) = &mut x;` binding obeys
  the same non-escape rules as today.

Internally, the existing normalisation in
`analyze_function::normalized_params` is the model — we generalise it:
the function body is type-checked with the binding's *visible* type set
to `T`, the binding's *declared* type set to `MutRef(T)`, and an
internal `is_through_ref` flag drives codegen. The legacy
`RirParamMode::Borrow`/`Inout` enums are retired because the same
information is now carried by the type pool.

### Interface-typed references

`Ref(I)` and `MutRef(I)`, where `I` names an interface, are made legal
parameter types. `resolve_param_type` is rewritten:

- Plain interface name `I` as parameter type: rejected with a
  diagnostic suggesting `Ref(I)` or `MutRef(I)` (replaces today's
  "use `borrow t: I`" hint).
- `Ref(I)` / `MutRef(I)`: routed through the interface ABI (fat
  pointer + vtable, ADR-0056) instead of the by-pointer ABI used for
  struct refs.

This closes the second gap from ADR-0062 Phase 8.

### Construction syntax: unchanged

`&x` and `&mut x` remain the only ways to construct `Ref(T)` / `MutRef(T)`
values. Implicit conversion ("auto-borrow") is still out of scope.

### Migration

This is a finishing ADR for ADR-0062, not a new feature behind a
preview flag. The change is staged so `make test` passes at every
phase boundary, but there is no user-visible preview gate — the new
form is already stable, and we are removing legacy spellings.

## Implementation Phases

- [x] **Phase 1: Pervasive `Self` resolution.** Introduce
      `Sema::current_self: Option<Type>` (and the matching
      `IfaceTy::SelfType` carry-through for interface bodies). Make
      `resolve_type` and `resolve_param_type` substitute `Self` at
      every depth. Delete the ad-hoc `if type_str == "Self"`
      branches in `sema/analysis.rs`. Add spec tests covering
      `Vec(Self)`, `Ref(Self)`, `MutRef(Self)`, `Option(Self)`,
      `(Self, Self)`, and `[Self; N]` in parameter, return, and
      local-binding positions.

- [ ] **Phase 2: Interface-typed references.** Rewrite
      `resolve_param_type` to accept `Ref(I)` / `MutRef(I)` for
      interface names `I` and dispatch through the ADR-0056 fat-pointer
      ABI. Add spec tests showing an interface-typed parameter using
      both `Ref(I)` and `MutRef(I)` with a struct conformer. Re-point
      the existing "use `borrow`/`inout`" diagnostic to suggest the
      new form.

- [ ] **Phase 3: Bare-name write-through, specified.** Add normative
      paragraphs to `docs/spec/src/06-items/01-functions.md` defining
      `name = e` as write-through when `name` has type `MutRef(T)`.
      Add spec tests for the previously underspecified scalar-MutRef
      case (`fn set(p: MutRef(i32), v: i32) { p = v }`) and for the
      same pattern via locals
      (`let r: MutRef(i32) = &mut x; r = 7;`). The implementation
      already handles parameter-position scalar through-write via
      normalisation; this phase ensures locals follow the same rule
      and that the spec matches the implementation.

- [ ] **Phase 4: Code-base codemod.** Mechanical sweep of
      `crates/gruel-spec/cases/`, `crates/gruel-ui-tests/cases/`,
      `scratch/`, ADR examples, and the spec markdown. Convert:
      - `borrow x: T` → `x: Ref(T)`
      - `inout x: T` → `x: MutRef(T)`
      - `borrow expr` → `&expr`
      - `inout expr` → `&mut expr`
      - `&self` / `borrow self` → `self: Ref(Self)`
      - `&mut self` / `inout self` → `self: MutRef(Self)`
      Run `make test` after the sweep; expect a green tree against the
      old compiler that still accepts both forms.

- [ ] **Phase 5: Remove receiver-mode sugar from the parser.** Drop
      the `&self` / `&mut self` / `borrow self` / `inout self`
      branches from `self_param_parser`. `SelfParam` becomes the
      identifier `self` plus an optional `: T` annotation. Update
      `Method`, `MethodSig`, and `DropFn` constructors. Run the test
      suite (already codemodded in Phase 4).

- [ ] **Phase 6: Remove `borrow` / `inout` keywords.** Delete
      `TokenKind::Borrow` / `TokenKind::Inout` from
      `gruel-lexer`. Delete the keyword-mode branches from
      `params_parser` and the call-site arg-mode parsers. The lexer
      now treats `borrow` / `inout` as plain identifiers.

- [ ] **Phase 7: Collapse internal modes.** Remove
      `SelfMode::Borrow`, `SelfMode::Inout`, `ParamMode::Borrow`,
      `ParamMode::Inout`, `RirParamMode::Borrow`,
      `RirParamMode::Inout`, `RirArgMode::Borrow`,
      `RirArgMode::Inout`, and the corresponding AIR variants. The
      borrow checker, place tracker, and codegen read the
      ref-ness from the type pool (`TypeKind::Ref` /
      `TypeKind::MutRef`) instead of from a parallel mode enum. The
      normalisation block in `analyze_function` becomes a no-op and
      is deleted; bindings keep their `Ref(T)` / `MutRef(T)` types
      end-to-end and the lowering happens once, at codegen, where
      a `MutRef(T)` parameter becomes an LLVM pointer with the same
      `noalias` / `nocapture` attributes today's `Inout` mode emits.

- [ ] **Phase 8: Spec rewrite and ADR closeout.** Rewrite
      `06-items/01-functions.md` and `06-items/05-interfaces.md` to
      remove every `borrow` / `inout` mention, replace
      `&self` / `&mut self` examples with the explicit form, and
      promote the "Migration note" paragraph to a "Historical note"
      pointing at this ADR. Update ADR-0013, ADR-0056, and ADR-0062
      with `superseded-by: 0076` (where surface-syntax sections are
      affected; ADR-0013's semantic content is preserved). Mark this
      ADR `status: implemented`. Run `make test` and the
      traceability check.

## Consequences

### Positive

- **One surface form for borrowing.** The user-facing language has a
  single answer to "how do I take a reference?": `Ref(T)` / `MutRef(T)`
  for types, `&x` / `&mut x` for values.
- **`Self` actually substitutes everywhere.** `Vec(Self)`,
  `Ref(Self)`, `Option(Self)`, `(Self, Self)`, `[Self; N]` all just
  work, including in interface methods and derives.
- **Two keywords reclaimed.** `borrow` and `inout` become available
  identifiers again.
- **Smaller compiler.** Several mode enums collapse, the
  normalisation block in `analyze_function` disappears, and the
  ad-hoc `Self`-string branches in sema are replaced by one
  context lookup.
- **Bare-name write-through is documented.** The spec finally says
  what the implementation has been doing since ADR-0062 Phase 8, and
  extends it to local bindings.

### Negative

- **Heavy churn.** Phase 4 touches every spec test, UI test,
  scratch program, and ADR example that still uses the keyword form.
  Comparable in size to ADR-0062 Phase 6.
- **Method receiver declarations get longer.** `&self` (5 chars)
  becomes `self: Ref(Self)` (15 chars). The win is uniformity, not
  brevity.
- **Three older ADRs (0013, 0056, 0062) gain `superseded-by` edges.**
  Their semantic content is unchanged, but readers must follow the
  chain to find the current surface syntax.

### Neutral

- **No new runtime behaviour.** Through-write semantics are unchanged
  from ADR-0062's normalised form; we are tightening the spec, not the
  implementation.
- **Codegen output is identical.** `MutRef(T)` lowers to the same
  LLVM pointer + attributes that `inout x: T` lowers to today.
- **No new preview flag.** The existing form is already stable; this
  ADR is removal work.

## Open Questions

1. **Should `let r: MutRef(T) = &mut x;` always treat subsequent
   `r = e` as write-through, even if the binding were declared
   `let mut r: MutRef(T) = ...`?**
   Proposal: yes — the rule is "type-driven, not binding-mutability
   driven", because `r` cannot meaningfully be rebound to a different
   ref (refs are scope-bound and not first-class storable). If a user
   writes `let mut r: MutRef(T) = &mut x; r = &mut y;`, the right-hand
   side has type `MutRef(T)` and we'd need to choose between rebind
   and write-through. We pick write-through universally and reject
   the rebind form with a clear diagnostic. Confirm during Phase 3.

2. **`Self` in free functions.** Inside a method (defined within a
   `struct` / `enum` body) `Self` is defined; inside a top-level
   free function, it is not. What about a free function in the same
   file as a single struct decl — should `Self` resolve there?
   Proposal: no. `Self` requires an enclosing struct / enum /
   interface / derive / drop-fn context, full stop. Mirrors Rust.

## Future Work

- **Lifetimes for stored references.** Out of scope here, exactly as
  in ADR-0062. This ADR's collapsing of modes-into-types makes the
  future addition of lifetime parameters strictly easier — there is
  one `Ref` type to extend, not a parallel mode and type system.
- **Auto-borrow at call sites.** Still deferred. Explicit `&` /
  `&mut` remains required.

## References

- ADR-0013: Borrowing Modes (semantic content preserved; surface
  syntax superseded by 0062 and finally retired by this ADR)
- ADR-0056: Structural Interfaces (interface-by-pointer ABI; the
  parameter-mode requirement on interface params is what Phase 2
  removes)
- ADR-0060: Complete Interface Signatures (defines `Self` /
  `IfaceTy::SelfType` for interface bodies; Phase 1 reuses this)
- ADR-0061: Generic Pointer Types (`BuiltinTypeConstructor`
  registry that hosts `Ref` / `MutRef`)
- ADR-0062: Reference Types Replacing Borrow Modes (parent ADR; this
  ADR closes its Phase 8 gaps)

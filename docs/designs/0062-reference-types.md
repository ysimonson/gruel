---
id: 0062
title: Reference Types Replacing Borrow Modes
status: implemented
tags: [types, syntax, ownership, borrowing, comptime]
feature-flag: reference_types
created: 2026-04-26
accepted: 2026-04-26
implemented: 2026-04-27
spec-sections: ["6.1"]
superseded-by: 0076
---

# ADR-0062: Reference Types Replacing Borrow Modes

## Status

Implemented (new surface form stable; old `borrow`/`inout` keywords retained as legacy aliases — see Phase 8 note)

## Summary

Replace ADR-0013's parameter passing modes (`borrow x: T`, `inout x: T`) with parameter *types* `Ref(T)` and `MutRef(T)`, constructed at call sites with Rust-style `&x` and `&mut x` expressions. Refs remain scope-bound — they cannot be stored in fields, returned, or escape — so this is a syntactic and structural unification, not the introduction of lifetimes. The companion ADR-0061 introduces the `BuiltinTypeConstructor` infrastructure this ADR reuses for `Ref`/`MutRef`.

## Context

ADR-0013 chose to model borrows as parameter modes ("Borrowing is a calling convention, not a type constructor"). That choice keeps the type system simple but creates two pieces of friction:

1. **Surface inconsistency.** The user writes `Ptr(T)` (after ADR-0061), `Vec(T)`, `Option(T)` — but `borrow x: T` for borrows. Three different syntactic forms for "a kind of T".
2. **Dead end for stored references.** If Gruel ever wants `Ref(T)` in struct fields or return positions, borrows-as-modes can't extend there. Borrows-as-types, even when scope-bound today, is the on-ramp.

The semantic content of ADR-0013 (no mutation, no move-out, no escape, exclusivity between mutable and immutable access) is sound. This ADR keeps the rules and changes how they're expressed.

## Decision

### New types: `Ref(T)` and `MutRef(T)`

Parameter modes `borrow` and `inout` become parameter types via the `BuiltinTypeConstructor` registry (ADR-0061). At call sites, `&expr` constructs a `Ref(T)` and `&mut expr` constructs a `MutRef(T)`.

```gruel
// Before
fn print_summary(borrow data: BigData) -> i32 {
    data.field1 + data.field2
}

fn append_byte(inout buf: Buf, b: u8) { ... }

fn main() -> i32 {
    let d = BigData { field1: 10, field2: 32 };
    let r = print_summary(borrow d);
    let mut buf = Buf::new();
    append_byte(inout buf, b'!');
    r + d.field1
}
```

```gruel
// After
fn print_summary(data: Ref(BigData)) -> i32 {
    data.field1 + data.field2
}

fn append_byte(buf: MutRef(Buf), b: u8) { ... }

fn main() -> i32 {
    let d = BigData { field1: 10, field2: 32 };
    let r = print_summary(&d);
    let mut buf = Buf::new();
    append_byte(&mut buf, b'!');
    r + d.field1
}
```

### Semantics — unchanged from ADR-0013

- A `Ref(T)` cannot be mutated through, moved out of, or escape its scope.
- A `MutRef(T)` is exclusive — at most one live `MutRef` to a place at a time, and no concurrent `Ref`s.
- Refs cannot be stored in struct fields, returned from functions, or captured by closures that outlive the function.
- Field projection works: from `Ref(Pair)` you can read `.a`.
- Method receivers: `fn read(self: Ref(Self))` and `fn mutate(self: MutRef(Self))`. Sugar `&self` / `&mut self` is also accepted.

### Construction syntax: Rust-style `&x` / `&mut x`

`&expr` and `&mut expr` are prefix expressions yielding `Ref(T)` / `MutRef(T)`. Mutability is **explicit** — `&x` always produces `Ref(T)`, never `MutRef(T)`, regardless of whether `x` is bound by `let` or `let mut`. This preserves ADR-0013's "explicit at call site" principle.

The `&` prefix form is contextually unambiguous with bitwise-and: prefix in expression position, infix between two operands.

`&` and `&mut` may appear anywhere a value of the target type is expected — argument position, the right side of a `let`, etc. — but the resulting `Ref`/`MutRef` value is still subject to the non-escape rules. So `let r = &x; f(r);` is permitted, but `let r = &x; return r;` is rejected because `r`'s type forbids escape.

### What this ADR does NOT include

- **Lifetimes**: refs remain scope-bound; this is not Rust's `&'a T`.
- **Stored references** in struct fields or return types — out of scope. A future ADR could relax this once lifetimes (or another mechanism) exist.
- **Pinned references** (per ADR-0013 future work).
- **Auto-borrow**: callers must write `&x` explicitly.

### Implementation shape

Introduce `TypeKind::Ref(TypeId)` and `TypeKind::MutRef(TypeId)`. Register `Ref` and `MutRef` in the `BuiltinTypeConstructor` registry from ADR-0061. The borrow checker (today: a sweep over `ParamMode::Borrow`/`Inout`) is reformulated to operate on values of `Ref`/`MutRef` types — same rules, different trigger. Codegen is unchanged: refs lower to LLVM pointers with the same calling-convention attributes today's borrows use.

### Migration

Cut over once feature-complete (matches ADR-0061's approach):

1. Implement new syntax behind the `reference_types` preview flag, with old `borrow`/`inout` modes still accepted (parallel grammars).
2. Codemod the test suite, scratch programs, ADR examples.
3. Remove old syntax in the same commit that stabilizes the feature.

## Implementation Phases

This ADR depends on ADR-0061 Phase 1 (builtin type-constructor registry) being complete.

- [x] **Phase 1: Type system** — introduce `TypeKind::Ref(TypeId)` and `TypeKind::MutRef(TypeId)` with intern-pool support, mirroring the existing pointer pool pattern.
- [x] **Phase 2: Parser** — accept `Ref(T)` / `MutRef(T)` as type expressions (via the constructor registry from ADR-0061). Accept `&expr` and `&mut expr` as prefix expressions. Gate behind the `reference_types` preview flag.
- [x] **Phase 3: Borrow checker port** — adapt ADR-0013's exclusivity, non-escape, and no-mutate rules to operate on values of `Ref`/`MutRef` types instead of parameter modes. Bidirectional during migration: `borrow x: T` and `x: Ref(T)` produce identical AIR for the body of the borrow checker.
- [x] **Phase 4: Method receivers** — accept `self: Ref(Self)` and `self: MutRef(Self)`, plus the `&self` / `&mut self` sugar.
- [x] **Phase 5: Codegen** — confirm refs lower identically to today's borrows (LLVM pointer with appropriate `noalias`/`readonly` attrs). Verify with the test suite.
- [x] **Phase 6: Codemod** — convert all `borrow x: T` → `x: Ref(T)`, `inout x: T` → `x: MutRef(T)`, `borrow expr` → `&expr`, `inout expr` → `&mut expr`. Touches spec tests, UI tests, scratch programs, ADR examples. *(Phase 6 lands a representative parallel test demonstrating the new syntax; the full sweep is bundled with phase 8 because two pre-existing limitations make a one-shot codemod infeasible: through-assignment tests like `a = b` on a `MutRef`-typed param need a deref operator that doesn't exist yet, and interface-typed params (`Sized(i32)`) require special ABI handling that doesn't compose with `Ref(...)`. Phase 8 deals with these alongside keyword removal so the test suite reaches a single coherent state.)*
- [x] **Phase 7: Spec rewrite** — update `docs/spec/src/06-items` and any borrow mentions in chapters 04/05. Mark ADR-0013's surface-syntax sections as superseded by this ADR.
- [x] **Phase 8: Remove old syntax and stabilize** — drop `ParamMode::Borrow` / `ParamMode::Inout`; remove the `borrow` and `inout` keywords; remove all `require_preview()` calls for `reference_types` and the `PreviewFeature::ReferenceTypes` enum variant. Update ADR status to `implemented`. *(Partial. The `require_preview()` call and `PreviewFeature::ReferenceTypes` variant are removed — the new surface form (`Ref(T)` / `MutRef(T)` / `&x` / `&mut x` / `&self` / `&mut self`) is stable. The through-read / through-write gap is closed: a parameter typed `Ref(T)` or `MutRef(T)` is normalized to `(T, Borrow)` / `(T, Inout)` inside `analyze_function`, so field projection (`p.x`), indexing (`arr[i]`), scalar reads (`r * 2`), and through-assignment via lvalue (`arr[i] = v`, `s.field = v`) all work the same as the legacy keyword forms. Method receivers desugared from `&self` / `&mut self` plumb their mode through, undo the receiver move at the call site, and pass by pointer; method bodies can also write `self.field = ...` since the assign-target parser accepts `self` as a base. `ParamMode::Borrow` / `Inout` and the `borrow` / `inout` keywords are still retained as legacy aliases because two narrower language gaps remain: a bare deref operator (`*r = v` to assign through a scalar `MutRef(i32)`), and ABI handling for `Ref(T)`-wrapped interface types. A follow-up ADR should land deref + interface-Ref ABI, then drop the legacy aliases as a final cleanup.)*

## Consequences

### Positive
- **Uniform surface form** for refs alongside `Ptr`/`MutPtr`/`Vec`/`Option`.
- **Two keywords removed** (`borrow`, `inout`).
- **On-ramp for stored references**: a future "lifetimes for refs" ADR is a natural extension rather than a re-architecture.
- **Cleaner mental model**: indirect access is "a kind of type", not "a kind of parameter".

### Negative
- **Heavy churn**: every spec test, UI test, and example with borrows is rewritten.
- **`&` becomes a new prefix operator**: a notable break from Gruel's keyword preference. Users coming from Rust will recognize it instantly; users coming from elsewhere lose the visual `borrow`/`inout` markers.
- **ADR-0013 partially superseded**: it keeps its semantic content but its surface-syntax sections become historical.
- **Slightly more characters per use** at parameter declarations (`x: Ref(BigData)` vs `borrow x: BigData`).

### Neutral
- **No semantic change**: borrow rules are identical.
- **No codegen change**: refs lower like borrows.

## Resolved Questions

1. **Construction syntax** — `&x` / `&mut x` (Rust-style, explicit mutability).
2. **Naming** — `Ref` / `MutRef` (parallels `Ptr` / `MutPtr` from ADR-0061).
3. **Method receiver sugar** — accept both `self: Ref(Self)` / `self: MutRef(Self)` and the short `&self` / `&mut self` forms.
4. **Is `&x` a general expression or only in argument position?** General expression — works in `let` bindings too. Non-escape rules still apply via the type.
5. **Migration approach** — parallel grammars during phases 1–7, cut over in phase 8.

## Open Questions

None.

## Future Work
- **Lifetimes for stored references** — would lift the non-escape rule, allowing `Ref(T)` in struct fields and return types. Big design space (lifetime inference, variance, etc.). Strict superset of this ADR.
- **Pinned references** (per ADR-0013 future work).
- **Auto-borrow** — convenience: callers don't write `&` if the parameter type is a `Ref`/`MutRef`. Loses call-site explicitness; defer.

## References
- ADR-0008: Affine Types and Mutable Value Semantics
- ADR-0013: Borrowing Modes
- ADR-0020: Built-in Types as Structs
- ADR-0025: Comptime
- ADR-0061: Generic Pointer Types (companion ADR; provides the `BuiltinTypeConstructor` infrastructure)
- [Hylo Language Tour: Functions](https://docs.hylo-lang.org/language-tour/functions-and-methods)

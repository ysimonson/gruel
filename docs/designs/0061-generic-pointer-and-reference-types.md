---
id: 0061
title: Generic Pointer and Reference Types
status: proposal
tags: [types, syntax, ownership, borrowing, pointers, comptime]
feature-flag: generic_indirection_types
created: 2026-04-26
accepted:
implemented:
spec-sections: ["6.1", "9.1"]
superseded-by:
---

# ADR-0061: Generic Pointer and Reference Types

## Status

Proposal

## Summary

Replace Gruel's two pieces of bespoke "indirect-access" grammar with comptime-generic types:

- `ptr const T` / `ptr mut T` (ADR-0028) become `Ptr(T)` / `MutPtr(T)`.
- `borrow x: T` / `inout x: T` parameter modes (ADR-0013) become parameter *types* `x: Ref(T)` / `x: MutRef(T)`, with construction expressions `&x` / `&mut x` at call sites.

Refs remain scope-bound — they cannot be stored in fields, returned, or escape — so this is a syntactic and structural unification, not the introduction of lifetimes.

## Context

Gruel today has two ad-hoc surface forms for talking about indirect access:

1. **Pointer keyword syntax** (ADR-0028): `ptr const T`, `ptr mut T`. Special-cased in the parser (`TypeExpr::PointerConst`/`PointerMut`) and in the type system (`TypeKind::PtrConst`/`PtrMut`).
2. **Borrow parameter modes** (ADR-0013): `borrow x: T`, `inout x: T`. These are *not* types — they're a `ParamMode` enum on parameters and call arguments. ADR-0013 explicitly states: "Borrowing is a calling convention, not a type constructor."

Meanwhile, ADR-0025 (comptime) produces a third form for parameterized types: `Vec(comptime T: type)`-style functions returning `type`. That form is now mature enough to express the standard library:

```gruel
fn Vec(comptime T: type) -> type {
    struct { ptr: ptr mut T, len: u64, cap: u64 }
}
```

The friction is presentational: a Gruel programmer writing generic code touches three different syntactic forms for what feels like the same concept ("a kind of T"). ADR-0028 already noted as a design goal that pointers should "work with existing type system features (comptime generics, borrow modes)" — this ADR finishes that thought by making them *the same kind of thing* as user-defined generic types.

## Decision

Two independent changes, gated behind a single preview feature `generic_indirection_types` for clarity but implementable separately.

### Change A: Pointer types as generics

Replace `ptr const T` / `ptr mut T` with `Ptr(T)` / `MutPtr(T)`.

```gruel
// Before
checked {
    let p: ptr mut i32 = @int_to_ptr(0x1000);
    let next: ptr mut i32 = @ptr_offset(p, 1);
}

// After
checked {
    let p: MutPtr(i32) = @int_to_ptr(0x1000);
    let next: MutPtr(i32) = @ptr_offset(p, 1);
}
```

**Implementation shape (recommended — Option A1):** introduce a `BuiltinTypeConstructor` registry alongside `BUILTIN_TYPES` in `gruel-builtins`. Each entry has a name (`Ptr`, `MutPtr`), arity (1), and a function mapping type arguments to an existing `TypeKind`. The parser sees `Ptr(i32)` as an ordinary call in type position; sema resolves the callee against the constructor registry and produces `TypeKind::PtrConst(intern(i32))` — i.e. the **internal IR is unchanged**, only the surface syntax changes.

**Alternative (Option A2):** define `Ptr` / `MutPtr` as user-visible functions in a prelude, e.g. `fn Ptr(comptime T: type) -> type { @intrinsic_ptr_const(T) }`. More uniform, but requires a "comptime intrinsic returning a type" mechanism. Defer to future work.

All pointer intrinsics (`@ptr_read`, `@ptr_write`, `@ptr_offset`, `@raw`, `@raw_mut`, `@null_ptr`, `@is_null`, `@int_to_ptr`, `@ptr_to_int`, `@ptr_copy`, `@syscall`) and all `checked`-block enforcement are unchanged. The `ptr` keyword is removed.

### Change B: Reference types replacing borrow modes

Replace `borrow x: T` / `inout x: T` parameter modes with parameter types `Ref(T)` / `MutRef(T)`. Replace `borrow expr` / `inout expr` call-site keywords with construction expressions `&expr` / `&mut expr`.

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

**Semantics — unchanged from ADR-0013:**
- A `Ref(T)` cannot be mutated, moved out of, or escape its scope.
- A `MutRef(T)` is exclusive — at most one live `MutRef` to a place, and no concurrent `Ref`s.
- Refs cannot be stored in struct fields, returned from functions, or captured by closures that outlive the function.
- Field projection works: from `Ref(Pair)` you can read `.a` (no copy unless it's `@copy`).
- Method receivers: `fn read(self: Ref(Self))` and `fn mutate(self: MutRef(Self))`. A short sugar form (e.g. `fn read(&self)` / `fn mutate(&mut self)`) is desirable but is left as an open question.

**What's new:** `&expr` and `&mut expr` are construction expressions. They yield a value of type `Ref(T)` / `MutRef(T)` whose lifetime is bounded by the enclosing statement / call. Sema enforces non-escape at *use sites of `Ref`/`MutRef` types*, instead of at *parameter mode boundaries*.

This is **not** Rust's `&T`: there are no lifetime annotations, refs cannot be stored, and they can only meaningfully appear in parameter and local positions.

**Implementation shape:** introduce `TypeKind::Ref(TypeId)` / `TypeKind::MutRef(TypeId)`. The borrow checker (today: a sweep over `ParamMode::Borrow`/`Inout`) is reformulated as a sweep over values of `Ref`/`MutRef` types, enforcing the same rules. Codegen is unchanged: refs lower to LLVM pointers, same as today's borrows.

### Construction syntax for refs (recommendation: `&` / `&mut`)

ADR-0013 deliberately put `borrow`/`inout` keywords at call sites for explicitness. With refs as types, we need a way to construct one. Three candidates:

| Form | Pros | Cons |
|------|------|------|
| `&d` / `&mut d` | Short, familiar (Rust, C), greppable as a single character | Re-uses `&` (currently bitwise-and only); breaks Gruel's keyword preference |
| `ref(d)` / `mut_ref(d)` | Keyword-style, consistent with Gruel's "spell things out" preference | Verbose; conflicts with potential future `ref` keyword |
| Implicit | Cleanest call sites | Loses ADR-0013's "explicit at call site" guarantee — you can't see at the call whether you're borrowing |

This ADR recommends `&d` / `&mut d`. Bitwise-and is contextually unambiguous (prefix vs. infix), and the gain in concision matters because every call site that today writes `borrow d` will now write the construction.

### Naming: `Ref`/`MutRef` vs `Borrow`/`Inout`

Two-word "MutRef" matches "MutPtr" for visual parallelism, and "ref" is the cross-language term for this concept. Keeping `Borrow`/`Inout` would preserve ADR-0013's vocabulary but breaks the parallel with `Ptr`/`MutPtr`. This ADR proposes `Ref`/`MutRef`; alternatives are listed in Open Questions.

### What this ADR does NOT include

- **Lifetimes**: Refs remain scope-bound. Any future "stored references" feature would be its own ADR building on this one.
- **Pinned places**: Future work, per ADR-0013.
- **Non-null pointer types**: Future work, per ADR-0028.
- **Custom user-defined type constructors**: The builtin constructor registry in Phase A1 is closed; users still write `fn Foo(comptime T: type) -> type { ... }`. Opening it is future work.

## Implementation Phases

Both changes are gated behind preview feature `generic_indirection_types`. Sub-phases are ordered so that each ends in a green tree.

### Change A: Generic pointer types

- [ ] **Phase A1: Builtin type-constructor infrastructure** — extend `gruel-builtins` with `BuiltinTypeConstructor { name, arity, lower: fn(&[TypeId]) -> TypeKind }`. Inject into the global namespace alongside `BUILTIN_TYPES`. No behavior change yet.
- [ ] **Phase A2: Parser/sema for `Ptr(T)` / `MutPtr(T)`** — accept call-style type expressions in type position when the callee resolves to a builtin constructor. Lower `Ptr(T)` to existing `TypeKind::PtrConst`. Gate behind the preview flag.
- [ ] **Phase A3: Diagnostics** — error/info messages display `Ptr(T)` / `MutPtr(T)` instead of `ptr const T` / `ptr mut T` when the new feature is enabled.
- [ ] **Phase A4: Codemod** — convert spec tests, UI tests, scratch programs, and ADR examples to the new syntax.
- [ ] **Phase A5: Spec rewrite** — update `docs/spec/src/09-runtime-behavior` (and any pointer mentions in 03-types) to document the new surface form. Mark ADR-0028's surface-syntax sections as superseded by this ADR.
- [ ] **Phase A6: Remove old syntax** — drop `TypeExpr::PointerConst`/`PointerMut` and the `ptr` keyword from the lexer and parser. Stabilize.

### Change B: Reference types

- [ ] **Phase B1: Type system** — introduce `TypeKind::Ref(TypeId)` and `TypeKind::MutRef(TypeId)` with intern-pool support; mirror existing pointer pool pattern.
- [ ] **Phase B2: Parser** — accept `Ref(T)` / `MutRef(T)` as type expressions (using the constructor registry from A1). Accept `&expr` and `&mut expr` as prefix expressions. Gate behind the preview flag.
- [ ] **Phase B3: Borrow checker port** — adapt ADR-0013's exclusivity / non-escape / no-mutate rules to operate on values of `Ref`/`MutRef` types instead of parameter modes. Bidirectional: `borrow x: T` and `x: Ref(T)` produce identical AIR for the body of the borrow checker.
- [ ] **Phase B4: Method receivers** — accept `self: Ref(Self)` and `self: MutRef(Self)`. Decide and implement sugar (`&self` / `&mut self` recommended).
- [ ] **Phase B5: Codegen** — confirm refs lower identically to today's borrows (LLVM pointer with appropriate noalias/readonly attrs). No expected work, but verify with the test suite.
- [ ] **Phase B6: Codemod** — convert all `borrow x: T` → `x: Ref(T)`, `inout x: T` → `x: MutRef(T)`, `borrow expr` → `&expr`, `inout expr` → `&mut expr`. Touches spec tests, UI tests, scratch, stdlib (when self-hosted parts emerge), and ADR examples.
- [ ] **Phase B7: Spec rewrite** — update `docs/spec/src/06-items` and any borrow mentions in 04/05. Mark ADR-0013's surface-syntax sections as superseded.
- [ ] **Phase B8: Remove old syntax** — drop `ParamMode::Borrow` / `ParamMode::Inout`; remove the `borrow` and `inout` keywords. Stabilize.

## Consequences

### Positive
- **Single grammar for indirect access**: `Ptr(T)`, `MutPtr(T)`, `Ref(T)`, `MutRef(T)`, `Vec(T)`, `Option(T)` all use the same call-style form.
- **Two pieces of bespoke grammar removed** (`ptr const`/`ptr mut`, `borrow`/`inout` modes).
- **Cleaner mental model for users**: indirect access is "a kind of type", not "a kind of parameter."

### Negative
- **Heavy churn**: every spec test, UI test, ADR example, and scratch program touching pointers or borrows is rewritten.
- **`&` becomes a new prefix operator**: a notable break from Gruel's keyword preference. Users coming from Rust will recognize it instantly; users coming from elsewhere lose the visual `borrow`/`inout` markers.
- **Two ADRs partially superseded**: ADR-0013 and ADR-0028 keep their semantic content but their surface-syntax sections become historical.
- **More to read in error messages**: `Ref(BigStruct)` is longer than `borrow BigStruct`.

### Neutral
- **No semantic change** under the recommended Option B1 (refs stay scope-bound).
- **Internal IR unchanged for pointers**; new variants for refs but lowering identical.

## Open Questions

1. **Construction syntax for refs** — `&` / `&mut` (recommended) vs. `ref(x)` / `mut_ref(x)` vs. implicit? The largest single break from Gruel's keyword style.
2. **Method receiver sugar** — should `&self` / `&mut self` be accepted as sugar for `self: Ref(Self)` / `self: MutRef(Self)`, or do we require the long form?
3. **Naming** — `Ref`/`MutRef` (parallels `Ptr`/`MutPtr`) vs `Borrow`/`Inout` (preserves ADR-0013 vocabulary)?
4. **Should `&` and `&mut` be expressions whose result type is inferred, or only valid in argument position?** Answering "only in argument position" simplifies the borrow checker but disallows `let r = &x; f(r);`. Strictly, ADR-0013 already disallows the latter (refs can't be in `let` bindings if they can't be returned-from? or can they?), so the choice may already be made.
5. **Should the builtin constructor registry (Phase A1) be opened to user code immediately, or kept closed?** Closed is simpler and matches today's "synthetic structs are compiler-only" posture. Open is more uniform.
6. **Spec test approach for the gradual migration** — do we ship the new syntax behind preview while the old syntax remains valid (parallel grammars), or do we cut over with a codemod once feature-complete? Cut over.

## Future Work
- **Non-null pointer types** `NonNullPtr(T)` (per ADR-0028 future-work).
- **Pinned references** (per ADR-0013 future-work).
- **User-defined type constructors** — let user code register types parameterized like the built-in `Ptr`/`Ref` (e.g., for representation tricks). Closed today; opening would be a separate ADR.

## References
- ADR-0008: Affine Types and Mutable Value Semantics
- ADR-0013: Borrowing Modes
- ADR-0020: Built-in Types as Structs
- ADR-0025: Comptime
- ADR-0028: Unchecked Code and Raw Pointers
- ADR-0042: Comptime Metaprogramming
- [Swift UnsafePointer](https://developer.apple.com/documentation/swift/unsafepointer)
- [Hylo Language Tour: Functions](https://docs.hylo-lang.org/language-tour/functions-and-methods)

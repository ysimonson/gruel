---
id: 0061
title: Generic Pointer Types
status: accepted
tags: [types, syntax, pointers, comptime]
feature-flag: generic_pointer_types
created: 2026-04-26
accepted: 2026-04-26
implemented:
spec-sections: ["9.1"]
superseded-by:
---

# ADR-0061: Generic Pointer Types

## Status

Accepted

## Summary

Replace Gruel's bespoke pointer keyword syntax (`ptr const T`, `ptr mut T`) with comptime-generic builtin types `Ptr(T)` and `MutPtr(T)`. The internal type representation is unchanged — `Ptr(T)` lowers to today's `TypeKind::PtrConst` and `MutPtr(T)` to `TypeKind::PtrMut`. This is purely a surface-syntax change, paired with new infrastructure (a closed `BuiltinTypeConstructor` registry) that ADR-0062 will reuse for `Ref`/`MutRef`.

## Context

Gruel currently has three syntactic forms for "a type parameterized by another type":

1. **Pointer keyword syntax** (ADR-0028): `ptr const T`, `ptr mut T`. Special-cased in the parser (`TypeExpr::PointerConst`/`PointerMut`) and the type system (`TypeKind::PtrConst`/`PtrMut`).
2. **Borrow parameter modes** (ADR-0013): `borrow x: T`, `inout x: T`. Not types — parameter modes only. Out of scope for this ADR; addressed by ADR-0062.
3. **Comptime generic functions** (ADR-0025): `fn Vec(comptime T: type) -> type { struct { ... } }`. The mature, user-facing form for everything except pointers and borrows.

The friction is presentational: a programmer writing generic code touches multiple syntactic forms for what feels like the same concept. ADR-0028 already noted as a design goal that pointers should "work with existing type system features (comptime generics, borrow modes)" — this ADR finishes that thought for pointers.

## Decision

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

### Implementation shape

Introduce a `BuiltinTypeConstructor` registry alongside `BUILTIN_TYPES` in `gruel-builtins`. Each entry has:

- `name: &'static str` — the type-constructor name (e.g. `"Ptr"`, `"MutPtr"`).
- `arity: usize` — number of comptime type arguments.
- `lower: fn(&[TypeId]) -> TypeKind` — function mapping type arguments to an existing `TypeKind`.

The parser sees `Ptr(i32)` as an ordinary call in type position. Sema resolves the callee against the constructor registry and produces `TypeKind::PtrConst(intern(i32))`. **The internal IR is unchanged** — only the surface syntax differs.

Alternative considered: define `Ptr` / `MutPtr` as user-visible functions in a prelude (`fn Ptr(comptime T: type) -> type { @intrinsic_ptr_const(T) }`). More uniform, but requires a "comptime intrinsic returning a type" mechanism. Deferred to future work.

### What does not change

- All pointer intrinsics (`@ptr_read`, `@ptr_write`, `@ptr_offset`, `@raw`, `@raw_mut`, `@null_ptr`, `@is_null`, `@int_to_ptr`, `@ptr_to_int`, `@ptr_copy`, `@syscall`).
- `checked`-block enforcement.
- Borrow modes (`borrow`/`inout`) — see ADR-0062 for that.
- Internal `TypeKind::PtrConst` / `TypeKind::PtrMut` representation.

### Migration

Cut over once feature-complete:

1. Implement the new syntax behind the `generic_pointer_types` preview flag.
2. Codemod the entire test suite, scratch programs, and ADR examples.
3. Once everything compiles on the new syntax, remove the old syntax in the same commit that stabilizes the feature.

## Implementation Phases

- [ ] **Phase 1: Builtin type-constructor infrastructure** — extend `gruel-builtins` with `BuiltinTypeConstructor { name, arity, lower }`. Inject into the global namespace alongside `BUILTIN_TYPES`. No behavior change yet (registry is empty/unused).
- [ ] **Phase 2: Parser/sema for `Ptr(T)` / `MutPtr(T)`** — accept call-style type expressions in type position when the callee resolves to a builtin constructor. Lower `Ptr(T)` to existing `TypeKind::PtrConst`, `MutPtr(T)` to `TypeKind::PtrMut`. Gate behind the `generic_pointer_types` preview flag.
- [ ] **Phase 3: Diagnostics** — error/info messages display `Ptr(T)` / `MutPtr(T)` instead of `ptr const T` / `ptr mut T` when the new feature is enabled. Tests in `gruel-ui-tests`.
- [ ] **Phase 4: Codemod** — convert spec tests, UI tests, scratch programs, and ADR examples to the new syntax. Old syntax remains accepted (parallel grammars) until Phase 6.
- [ ] **Phase 5: Spec rewrite** — update `docs/spec/src/09-runtime-behavior` (and any pointer mentions in `03-types`) to document `Ptr(T)` / `MutPtr(T)`. Mark ADR-0028's surface-syntax sections as superseded by this ADR.
- [ ] **Phase 6: Remove old syntax and stabilize** — drop `TypeExpr::PointerConst`/`PointerMut`, the `ptr` keyword from the lexer/parser, all `require_preview()` calls for `generic_pointer_types`, and the `PreviewFeature::GenericPointerTypes` enum variant. Update ADR status to `implemented`.

## Consequences

### Positive
- **Uniform surface form** for pointers and other generic types (`Vec(T)`, `Option(T)`, etc.).
- **Reusable infrastructure**: the `BuiltinTypeConstructor` registry is a building block for ADR-0062 (`Ref`/`MutRef`).
- **One less keyword** (`ptr`).

### Negative
- **Test churn**: every spec test, UI test, and example mentioning pointers is rewritten.
- **ADR-0028 partially superseded**: its surface-syntax sections become historical.
- **Slightly more characters per use** (`MutPtr(i32)` vs `ptr mut i32`).

### Neutral
- **No semantic change**: pointers work exactly as before.
- **No codegen change**: internal IR identical.

## Resolved Questions

1. **Should the builtin constructor registry be open to user code?** No — closed for now, matching today's "synthetic structs are compiler-only" posture. Opening it is future work and would be its own ADR.
2. **Migration approach?** Parallel grammars during phases 1–5, cut over in phase 6.

## Open Questions

None.

## Future Work
- **Non-null pointer types** `NonNullPtr(T)` (per ADR-0028 future-work).
- **User-defined type constructors** — let user code register types parameterized like the built-in `Ptr` (e.g., for representation tricks). Closed today; opening would be a separate ADR.

## References
- ADR-0020: Built-in Types as Structs
- ADR-0025: Comptime
- ADR-0028: Unchecked Code and Raw Pointers
- ADR-0042: Comptime Metaprogramming
- ADR-0062: Reference Types Replacing Borrow Modes (companion ADR)
- [Swift UnsafePointer](https://developer.apple.com/documentation/swift/unsafepointer)

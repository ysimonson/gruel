---
id: 0063
title: Pointer Operations as Methods on Ptr / MutPtr
status: proposal
tags: [pointers, intrinsics, syntax, builtins]
feature-flag: pointer_methods
created: 2026-04-27
accepted:
implemented:
spec-sections: ["9.1", "9.2"]
superseded-by:
---

# ADR-0063: Pointer Operations as Methods on Ptr / MutPtr

## Status

Proposal

## Summary

Move the pointer-manipulation intrinsics out of the global `@…` namespace and onto the `Ptr(T)` / `MutPtr(T)` types introduced by ADR-0061. Operations on a pointer value use ordinary method-call syntax (`p.read()`, `p.write(v)`); pointer constructors use a fully-applied path call on the type — `Ptr(T)::null()`, `Ptr(T)::from(&t)`, `Ptr(T)::from_int(addr)`. The address-of intrinsics `@raw` / `@raw_mut` are not renamed — they are *replaced* by the associated function `Ptr(T)::from(r: Ref(T)) -> Ptr(T)` (and the `MutPtr` analogue), which composes out of ADR-0062's `&x` / `&mut x` and a regular function call. After this ADR there are no remaining pointer intrinsics in the `@…` namespace.

## Context

ADR-0061 introduced `Ptr(T)` and `MutPtr(T)` as type-constructor types. The operations on those types still live in the flat `@intrinsic` namespace from ADR-0028:

| Today | Reads as |
|-------|----------|
| `@ptr_read(p)` | "global function ptr_read" |
| `@ptr_write(p, v)` | "global function ptr_write" |
| `@ptr_offset(p, n)` | "global function ptr_offset" |
| `@is_null(p)` | "global function is_null" |
| `@null_ptr()` | "global function null_ptr" |
| `@int_to_ptr(addr)` | "global function int_to_ptr" |
| `@ptr_to_int(p)` | "global function ptr_to_int" |
| `@ptr_copy(dst, src, n)` | "global function ptr_copy" |
| `@raw(x)` | "global function raw" |
| `@raw_mut(x)` | "global function raw_mut" |

The mismatch is real: the *type* is `Ptr(T)`, the *operations* on it are not on `Ptr`. Method syntax — `p.read()`, `Ptr(T)::null()` — locates each operation under its receiver type and parallels how `String` already works (`s.len()`, `String::new()`).

For the no-instance constructors (`null`, `from`, `from_int`) the path-call LHS is the *fully-applied* type — `Ptr(i32)::null()`, not `Ptr::null()`. This avoids introducing a separate name-resolution case for "constructor names in expression position" (`Ptr` alone is not a type, only `Ptr(i32)` is). The LHS goes through the existing type-call mechanism that ADR-0061 already established, the type parameter is bound by the syntax rather than recovered from the binding annotation, and the dispatch story collapses to one shape: path call on a fully-resolved type.

`@raw` / `@raw_mut` are an interesting subcase. They originally stayed global because they are *place-expression* operations — they take an lvalue and produce a pointer to its storage, which a regular value-receiving function cannot do (taking the address of a by-value parameter would point to a soon-to-die stack copy). With ADR-0062's `Ref(T)` / `MutRef(T)`, the place-tracking has a first-class home: `&x` constructs a `Ref(T)` over the original storage, and the associated function `Ptr(T)::from(r: Ref(T)) -> Ptr(T)` rewraps it. The address-of intrinsics dissolve into a borrow plus a function call — no special compiler magic needed at the address-of step.

## Decision

### New surface form

```gruel
checked {
    let p = MutPtr(i32)::from(&mut x);           // was: @raw_mut(x)
    let q = Ptr(i32)::from(&y);                  // was: @raw(y)

    let v: i32 = p.read();                       // was: @ptr_read(p)
    p.write(42);                                 // was: @ptr_write(p, 42)
    let r = p.offset(3);                         // was: @ptr_offset(p, 3)

    let n  = Ptr(u8)::null();                    // was: @null_ptr() w/ Ptr(u8) annotation
    let m  = MutPtr(u8)::null();                 // ditto, mutable variant
    let p2 = MutPtr(u8)::from_int(addr);         // was: @int_to_ptr(addr) w/ MutPtr(u8) annotation
    let a: u64 = q.to_int();                     // was: @ptr_to_int(q)

    if q.is_null() { ... }                       // was: @is_null(q)

    p.copy_from(q, 16);                          // was: @ptr_copy(p, q, 16)
}
```

Pointer construction no longer needs a binding annotation to fix `T` — the type-call LHS pins it directly.

### Method / associated-fn signatures

| Form | Receiver / static | Defined on | Signature |
|------|-------------------|------------|-----------|
| `p.read()` | method | `Ptr(T)`, `MutPtr(T)` | `(self) -> T` |
| `p.write(v)` | method | `MutPtr(T)` only | `(self, v: T) -> ()` |
| `p.offset(n)` | method | `Ptr(T)`, `MutPtr(T)` | `(self, n: i64) -> Self` |
| `p.is_null()` | method | `Ptr(T)`, `MutPtr(T)` | `(self) -> bool` |
| `p.to_int()` | method | `Ptr(T)`, `MutPtr(T)` | `(self) -> u64` |
| `p.copy_from(src, n)` | method | `MutPtr(T)` | `(self, src: Ptr(T) \| MutPtr(T), n: u64) -> ()` |
| `Ptr(T)::from(r)` | assoc fn | `Ptr(T)` | `(r: Ref(T)) -> Ptr(T)` |
| `MutPtr(T)::from(r)` | assoc fn | `MutPtr(T)` | `(r: MutRef(T)) -> MutPtr(T)` |
| `Ptr(T)::null()` | assoc fn | `Ptr(T)` | `() -> Ptr(T)` |
| `MutPtr(T)::null()` | assoc fn | `MutPtr(T)` | `() -> MutPtr(T)` |
| `Ptr(T)::from_int(addr)` | assoc fn | `Ptr(T)` | `(addr: u64) -> Ptr(T)` |
| `MutPtr(T)::from_int(addr)` | assoc fn | `MutPtr(T)` | `(addr: u64) -> MutPtr(T)` |

For the `from` cases, the LHS `Ptr(T)` and the argument's `Ref(T)` must have matching `T` — sema unifies them like any other generic call with an explicit type argument.

The same `checked` / `unchecked` block requirements ADR-0028 places on the intrinsics carry over verbatim — they are properties of the operation, not the spelling. `Ptr(T)::from(&x)` requires `checked` for the `Ptr(T)::from` step, exactly as today's `@raw(x)` does. The `&x` itself is unchecked — it is a regular borrow, post ADR-0062.

### Implementation shape

Today's `BuiltinTypeDef` system (`STRING_TYPE`, etc.) describes methods on a *concrete* type with non-generic parameter and return types. `Ptr(T)` / `MutPtr(T)` need methods whose signatures mention `T` (`read` returns `T`, `write` takes `T`, `offset` returns `Self`, `from` takes `Ref(T)`).

Two viable strategies; the ADR picks (A) and lists (B) as future work:

- **(A) New `BuiltinTypeConstructorMethods` side registry alongside the existing constructor registry.** Each entry binds a method or associated function to a type-constructor kind (`Ptr` or `MutPtr`) with a *signature template* that may mention `T` (the constructor's type parameter) and `Self`. Sema's method-call path consults this registry whenever the receiver type is `Ptr(_)` / `MutPtr(_)`; the path-call path consults it whenever the LHS resolves to such a type. Each entry maps to an existing `IntrinsicId` (no new runtime functions). One focused mechanism, no broader generics work required.

- **(B) Generalised `BuiltinTypeDef` for type-constructor types.** Extend `BuiltinTypeDef` so `name`, `fields`, `methods`, and `associated_fns` can be parameterised by a type variable. More uniform with `STRING_TYPE`, but a much bigger surface change with more open questions (how does the registry encode `Vec(T)`-shape types?). Deferred.

In addition, the parser needs a small grammar extension: a path call's LHS today is an `IDENT` (or path of idents); it needs to also accept a type-call expression. The new shape is `TypeExpr "::" IDENT "(" args ")"`, and once it's in place it works for any future structural type with associated functions, not just `Ptr` / `MutPtr`.

The intrinsic *implementations* in codegen and sema do not move — only the dispatch from surface syntax changes. Each method / assoc fn maps 1:1 to an existing `IntrinsicId` so codegen continues to treat the operation through the same path.

### Migration

Same pattern as ADR-0061 / ADR-0062:

1. Add the new method / assoc-fn forms behind `--preview pointer_methods`. All existing `@…` pointer intrinsics keep working in parallel.
2. Codemod the spec/UI tests, scratch programs, and learn pages to the new forms.
3. Drop every pointer intrinsic from `INTRINSICS` and from `IntrinsicId`: `@ptr_read`, `@ptr_write`, `@ptr_offset`, `@ptr_to_int`, `@int_to_ptr`, `@null_ptr`, `@is_null`, `@ptr_copy`, `@raw`, `@raw_mut`. The `@…` namespace ends up with zero pointer entries.
4. Stabilise the feature.

## Implementation Phases

- [x] **Phase 1: `BuiltinTypeConstructorMethods` registry** — introduce the registry in `gruel-intrinsics` (closer to `IntrinsicId`, no dep cycle) as `POINTER_METHODS` with `PointerKind` / `PointerOpForm` / `PointerMethod` types. Entries reference an existing `IntrinsicId`; no behavior change yet (registry is unused).
- [ ] **Phase 2: Method-call dispatch for `Ptr(T)` / `MutPtr(T)`** — when sema sees `p.method(...)` and `p`'s type is `Ptr(_)` / `MutPtr(_)`, look the method up in the registry from phase 1 and lower it as if the user had written the equivalent intrinsic call. Gate behind `--preview pointer_methods`.
- [ ] **Phase 3: Path call on a type-call LHS** — extend the parser so a path call accepts a `TypeExpr` (specifically a type-call) on the LHS, not just an ident. Sema evaluates the LHS to a `Type`; when the resolved type is `Ptr(_)` / `MutPtr(_)` it looks up the associated function in the registry from phase 1. Gated. After this phase `Ptr(i32)::null()`, `Ptr(i32)::from(&x)`, and `Ptr(i32)::from_int(addr)` all work.
- [ ] **Phase 4: Codemod** — convert spec/UI tests, scratch programs, ADR examples, and learn pages to the new syntax. `@raw(x)` becomes `Ptr(T)::from(&x)`, `@raw_mut(x)` becomes `MutPtr(T)::from(&mut x)`, value methods take method form, the remaining constructors take fully-applied path form. Each migrated test picks up `preview = "pointer_methods"` and `preview_should_pass = true`.
- [ ] **Phase 5: Spec rewrite** — update `docs/spec/src/09-unchecked-code/02-intrinsics.md` to document each operation in its method form; mark the old intrinsic names as historical aliases. ADR-0028's intrinsic table moves to a "see also" link to ADR-0063.
- [ ] **Phase 6: Remove old syntax and stabilize** — drop the `@ptr_read` / `@ptr_write` / `@ptr_offset` / `@ptr_to_int` / `@int_to_ptr` / `@null_ptr` / `@is_null` / `@ptr_copy` / `@raw` / `@raw_mut` intrinsic entries and their `IntrinsicId` variants; remove the `pointer_methods` preview gate and the `PreviewFeature::PointerMethods` enum variant. Update ADR status to `implemented`.

## Consequences

### Positive
- **Operations live with their receiver.** `p.read()` is discoverable from `Ptr(T)`'s docs; `Ptr(T)::null` is an associated function on `Ptr(T)`. The flat `@…` namespace ends up with zero pointer entries.
- **No special address-of intrinsic.** `@raw` / `@raw_mut` were the two `@…` operations that *had* to be intrinsic-shaped because they read a place. ADR-0062's `&x` already does that; `Ptr(T)::from` is then just a regular function. Less compiler magic, fewer entries in `IntrinsicId`.
- **No new name-resolution case.** Path calls on `Ptr(T)::name(...)` go through the existing type-call mechanism for the LHS — `Ptr(i32)` is already a real `Type`, just like `Vec(i32)` will be. Sema does not need to know that `Ptr` is a constructor in expression position; it only needs to look up an associated fn on a fully-resolved type.
- **`T` is always carried by the syntax.** Pointer construction never depends on a binding annotation to know its result type — `let p = Ptr(i32)::null();` works without `let p: Ptr(i32) = …;`. One way to write each construction.
- **Reusable pattern for future pointer types.** A `NonNullPtr(T)` per ADR-0028's future work plugs into the same registry without inventing new intrinsic spellings.
- **Surface-form symmetry with ADR-0061 / ADR-0062.** Together with `Ref`/`MutRef`, every pointer-shaped concept now has the same call shape: `Type(T)` for the type, methods on the value, `Type(T)::name(...)` for constructors.

### Negative
- **Test churn**: every test calling a pointer intrinsic is rewritten.
- **Construction is more verbose.** `@raw(x)` becomes `Ptr(T)::from(&x)`, `@null_ptr()` becomes `Ptr(T)::null()` — a few more characters per use. The borrow becomes visible (matching ADR-0062 elsewhere), and the type parameter becomes visible at the call site rather than implicit in the binding. Readers used to the intrinsic form will notice the extra step; readers coming from explicit-type-arg languages won't.
- **Ad-hoc dispatch side table**: `BuiltinTypeConstructorMethods` is a second method-resolution mechanism alongside `BuiltinTypeDef`'s methods, the user-defined struct method path, and the interface dispatch path. It is small and closed (only `Ptr` and `MutPtr` use it for now) but it is an extra place future work has to consider.
- **Parser change to path-call LHS.** Today the LHS of a `::method(args)` call is a single ident. Extending it to accept a type-call expression is a small grammar change, but does require care that the existing `Color::Red` style and the new `Ptr(i32)::null()` style coexist cleanly.
- **`copy_from` reads asymmetrically.** Today `@ptr_copy(dst, src, n)` puts `dst` first, matching `memcpy(dst, src, n)`. As a method, `dst.copy_from(src, n)` flips the surface order — readable, but a second-look moment for users who know the C convention.

### Neutral
- **No new runtime functions, no IR changes.** Each surface form maps to an existing `IntrinsicId` and goes through the same codegen path.
- **No change to checked/unchecked rules.** The block requirements move with the operation.

## Open Questions

1. **`copy_from` self-vs-arg ordering.** The ADR currently picks `dst.copy_from(src, n)` (dst is `self`, src is an arg). Alternative: `src.copy_into(dst, n)`. The `_from` convention matches Rust's `slice::copy_from_slice` and is the more common shape; the `_into` direction makes the call site read like a write through `src`, which feels wrong. Going with `copy_from`.
2. **Should `null` and `from_int` be available on both `Ptr` and `MutPtr`, or just one?** Today `@null_ptr()` is generic over the result type — the binding annotation picks `Ptr(T)` or `MutPtr(T)`. The proposal explicitly duplicates them as `Ptr(T)::null()` / `MutPtr(T)::null()` (and likewise for `from_int`) so the LHS type matches the result type. (No "the result type is determined elsewhere" magic.)
3. **`p.read()` on `MutPtr(T)` vs `Ptr(T)` only?** Today `@ptr_read` accepts both. The proposal keeps that — `MutPtr(T)` permits everything `Ptr(T)` does. An alternative is to require an explicit downcast `mp.as_const()` before reading; that is more pedantic than today's behavior and not justified by any incident.
4. **Should `Ptr(T)::from` accept a `MutRef(T)` too?** A `MutRef(T)` strictly knows more than a `Ref(T)` does, so `Ptr(T)::from(&mut x)` "should" type-check (you'd lose the mut, but the address is the same). The proposal doesn't allow this — `Ptr(T)::from` accepts `Ref(T)` only, `MutPtr(T)::from` accepts `MutRef(T)` only. If the user wants a const pointer to a mutable place they write `Ptr(T)::from(&x)` (which is perfectly legal even when `x: let mut`). Restricting the conversion keeps the call shape unambiguous.

## Future Work
- **`NonNullPtr(T)`** (ADR-0028 future work) — adds another constructor that reuses this registry.
- **Generalised `BuiltinTypeDef` for type-constructor types** — see strategy (B) in the Decision section. Worth revisiting when more parameterised builtins land (e.g. `Vec(T)`). The path-call-on-type-expression mechanism this ADR introduces in the parser is a prerequisite that lands here, so future generic builtins inherit it for free.
- **Trait-based method sharing** — `read` / `is_null` / `to_int` are duplicated across `Ptr` and `MutPtr`. A trait would dedupe them. Defers to whatever interface story Gruel settles on for builtins.

## References
- ADR-0028: Unchecked Code and Raw Pointers
- ADR-0061: Generic Pointer Types
- ADR-0062: Reference Types Replacing Borrow Modes
- ADR-0050: Closed-Enum Intrinsic Registry
- ADR-0020: Built-in Types as Structs

---
id: 0059
title: Drop and Copy as Interfaces — Replacing `linear` and `@copy`
status: proposal
tags: [types, ownership, interfaces, derives]
feature-flag: ownership_interfaces
created: 2026-04-26
accepted:
implemented:
spec-sections: ["3.8", "4.13"]
superseded-by:
---

# ADR-0059: Drop and Copy as Interfaces — Replacing `linear` and `@copy`

## Status

Proposal

## Summary

Reframe Gruel's ownership model around two compiler-recognized structural interfaces:

```gruel
interface Drop { fn drop(self); }
interface Copy { fn copy(borrow self) -> Self; }
```

A type's ownership posture is derived from interface conformance, with `Copy` and `Drop` mutually exclusive (a type that implements both is a compile error):

| Implements `Copy`? | Implements `Drop`? | Posture  |
|--------------------|--------------------|----------|
| yes                | **must not**       | `Copy`   |
| no                 | yes                | `Affine` |
| no                 | no                 | `Linear` |

User types opt in either by writing `fn drop(self)` / `fn copy(borrow self) -> Self` directly or via auto-derive (`@derive(Drop)`, `@derive(Copy)`) — both built on ADR-0058. The default for a plain `struct` is **linear** (no implicit drop, no implicit copy). Built-in primitives (`i32`, `bool`, floats, pointers, plain enums) and arrays of `Copy` elements get synthesized `Copy` conformance — matching Rust's defaults so primitives stay "free" to use.

The `linear` keyword and `@copy` directive are removed. `@ownership(T)` becomes a simple lookup of interface conformance.

## Context

### What ADR-0008 gave us, and where it strained

ADR-0008 introduced affine-by-default with two opt-ins (`@copy` directive, `linear` keyword). That worked, but the surface has grown three independent knobs for one concept:

- `@copy` (a directive) — bitwise-copyable
- `linear` (a keyword) — must-consume
- `fn drop(self)` (an inline method recognized by ADR-0053) — has cleanup logic

Each is parsed/checked by hand-written rules. New knobs (`@handle`, ADR-0008 §6.x; future `Hash`, `Eq`) would extend that pattern indefinitely.

### What's now available

Three recently-landed features make a uniform replacement possible:

- **ADR-0056 (structural interfaces).** Lets the compiler ask "does `T` have method set `I`?" without an `impl` declaration.
- **ADR-0057 (anonymous interfaces).** Lets us reference interfaces inline.
- **ADR-0058 (derives).** Lets users define `derive Drop { ... }` once and apply it via `@derive(Drop)` on any type. The bodies type-check at derive-definition time with `Self` free.

With these, "is this type droppable?" and "is this type copyable?" stop being compiler-special bits and become "does it conform to `Drop`?" / "does it conform to `Copy`?".

### Why default-linear

Today's default is affine (any struct can be implicitly dropped at end of scope). That's convenient, but:

- It silently invokes whatever drop logic the type's fields require. Resource leaks hide in "I forgot to consume that".
- It conflates "I want to write `fn drop(self)`" with "I want my type to be droppable" — they're the same thing today, which is why `@copy` had to grow a special "no `fn drop` allowed" rule.
- When `Drop` becomes a real interface, asking the user to *opt in* (`@derive(Drop)`) makes the cost of cleanup visible and uniform across every type.

The cost: every plain `struct Foo { x: i32 }` needs `@derive(Drop)` to be droppable. We accept that cost in exchange for one mental model.

## Decision

### Two compiler-recognized interfaces

`Drop` and `Copy` are defined in the standard prelude:

```gruel
interface Drop {
    fn drop(self);
}

interface Copy {
    fn copy(borrow self) -> Self;
}
```

The compiler recognizes both by name (registered in `KnownSymbols` alongside `String`, `Arch`, `Os`, `Ownership`). They are otherwise normal interfaces — anyone can take `comptime T: Drop` or hold a `Drop` runtime value.

### Conformance ⇒ ownership posture

For every type `T`:

- `T` is **`Copy`** iff `T` conforms structurally to `Copy`.
- `T` is **`Affine`** iff `T` conforms to `Drop`.
- `T` is **`Linear`** otherwise.

A type that simultaneously conforms to both `Copy` and `Drop` is rejected by sema with a multi-span diagnostic citing the conflicting methods. The hazard is the same one that motivates Rust's exclusion: `fn copy(borrow self) -> Self` produces multiple values from one, and a subsequent `fn drop(self)` would run on each of them — double-free territory. Going out of scope releases storage for `Copy` types as a no-op; cleanup logic only fires for `Drop` (affine) types.

The conformance check uses the existing structural mechanism from ADR-0056 — a struct/enum conforms iff its method set covers the required signatures. `fn drop(self)` and `fn copy(borrow self) -> Self` may be written inline (ADR-0053) or attached via `@derive(...)` (ADR-0058); both routes feed the same method list.

### Auto-derives

The standard library provides:

```gruel
derive Drop {
    fn drop(self) {
        comptime_unroll for f in @type_info(Self).fields {
            comptime if @ownership(f.field_type) == Ownership::Linear {
                @compile_error("@derive(Drop) on a struct with a linear field");
            } else if @ownership(f.field_type) == Ownership::Affine {
                drop(@field(self, f.name));
            }
            // Copy: no-op — storage released with the parent.
        }
    }
}

derive Copy {
    fn copy(borrow self) -> Self {
        comptime_unroll for f in @type_info(Self).fields {
            comptime if @ownership(f.field_type) != Ownership::Copy {
                @compile_error("@derive(Copy) requires every field to be Copy");
            }
        }
        // Build a new Self by copying every field (bitwise; primitives and
        // Copy structs all support implicit duplication at this point).
        Self { ...comptime_unroll for f in @type_info(Self).fields { f.name: @field(self, f.name) } }
    }
}
```

(Exact body syntax is sketched; the working version lands in Phase 4. The point is that ADR-0058 already supports it — no new compiler machinery is needed for the derive expansion itself.)

The two rules:

- **`@derive(Drop)`**: every field must be `Copy` or `Drop`. `Copy` fields are skipped (no cleanup needed); `Drop` fields are recursively dropped. A linear field is rejected with a diagnostic citing the `@derive(Drop)` site and the offending field. (Manual `fn drop(self)` is *not* subject to this rule — see below.)
- **`@derive(Copy)`**: every field must be `Copy`. A `Drop` or linear field is rejected. This is the same rule that applies to manual `fn copy(borrow self) -> Self` — see "Hand-written" section.

When the user writes:

```gruel
@derive(Drop)
struct Buffer { name: String, capacity: i32 }  // String: Drop, i32: Copy
```

`name` is recursively dropped; `i32` is skipped. The struct conforms to `Drop` and is therefore affine. This is the "trigger `@compile_error` if their members prevent this struct from being affine or copy" requirement: it fires *only* in the cases where the field's posture is incompatible with the chosen derive.

### Hand-written `fn drop` / `fn copy` (no derive)

A struct may implement either interface manually:

```gruel
struct FileHandle {
    fd: i32,

    fn drop(self) {
        unchecked { @syscall(SYS_close, self.fd as i64); }
    }
}
```

This struct conforms to `Drop` structurally — no `@derive` needed. It is therefore affine. The same applies to `fn copy(borrow self) -> Self` for types that want non-trivial Copy semantics (e.g. atomic-refcount bump on a `Copy` reference-counted handle).

**Manual `fn drop` with a linear field is allowed.** Inside `fn drop(self)`, `self` is consumed by the call, so the body may destructure it and pass each linear field along to a consumer:

```gruel
struct Resource {
    handle: Token,   // hypothetical linear type

    fn drop(self) {
        let Resource { handle } = self;
        close_token(handle);   // consumes the linear Token
    }
}
```

No special rule is needed — the existing per-body linearity check fires normally. If the body fails to consume a linear binding, the user gets the standard "linear value not consumed" diagnostic. The auto-derive's stricter "no linear fields" rule applies only to `@derive(Drop)` because the synthesized body doesn't know *how* to consume them.

**Manual `fn copy` is subject to the same field-posture rules as `@derive(Copy)`.** The implementation cannot dodge the structural check by hand-rolling its own body: a `Copy` type implies *implicit* duplication at every use site, and an implicitly-multiplied `Drop` or linear field is the double-free / unbounded-resource hazard that motivates `Copy ⊥ Drop` in the first place. Sema therefore rejects any type that conforms to `Copy` while having a `Drop` or linear field, with the same diagnostic regardless of whether the `fn copy` came from a derive or was written by hand. This is the unified rule:

> A type conforms to `Copy` only if every field is `Copy`. Violations are reported with a multi-span diagnostic citing the conformance site (the inline `fn copy` or the `@derive(Copy)` directive) and the offending field.

For *explicit-duplication* semantics on a non-`Copy` resource (the case ADR-0008's `@handle` directive previously addressed), simply write an inline duplicator under whatever name fits the type:

```gruel
struct Rc {
    ptr: ptr mut u8,

    fn drop(self) { /* dec refcount, free if zero */ }
    fn handle(borrow self) -> Self { /* inc refcount, return Self { ptr: self.ptr } */ }
}
```

The struct stays affine (it conforms to `Drop`); `.handle()` is just a regular method. No directive is needed — the directive's only job under ADR-0008 was to validate the method's signature, which falls out of normal type checking. `@handle` is therefore removed in Phase 6 alongside `@copy` and `linear`.

### Built-in conformance (the Rust-style defaults)

The compiler injects synthetic `Copy` *or* `Drop` conformance for built-in types so primitives and pointers stay free of ceremony. Per the mutual-exclusion rule, a built-in type gets exactly one of the two:

| Type                                                   | Conforms to |
|--------------------------------------------------------|-------------|
| `i8`…`i64`, `u8`…`u64`, `isize`, `usize`               | `Copy`      |
| `f16`, `f32`, `f64`                                    | `Copy`      |
| `bool`                                                 | `Copy`      |
| `()`                                                   | `Copy`      |
| `!` (Never)                                            | `Copy`      |
| `ptr const T`, `ptr mut T`                             | `Copy`      |
| `[T; N]` where `T: Copy`                               | `Copy`      |
| `[T; N]` where `T: Drop`                               | `Drop`      |
| `[T; N]` where `T` is linear                           | linear      |
| Tuples of `Copy`                                       | `Copy`      |
| Tuples containing any `Drop`                           | `Drop`      |
| `enum E { ... }` where every variant payload is `Copy` | `Copy`      |
| `enum E { ... }` containing any `Drop` payload         | `Drop`      |
| `String`                                               | `Drop`      |
| `comptime_int`, `comptime_str`, `type`                 | `Copy`      |

Synthesis happens during built-in injection (the same place `inject_builtin_types` runs today). User code sees these types as ordinary `Copy`/`Drop` conformers — `@ownership(i32) == Ownership::Copy`, `@ownership(String) == Ownership::Affine`, etc.

End-of-scope handling: `Copy`-only built-ins release storage with no destructor call; `Drop`-only built-ins (or composites containing them) get their `drop` invoked. The two paths never overlap.

### `@ownership` becomes a thin wrapper

`@ownership(T)` is now defined as:

```
if T : Copy then Ownership::Copy
else if T : Drop then Ownership::Affine
else Ownership::Linear
```

The current implementation already produces these answers via `is_type_copy`/`is_type_linear`; under this ADR those helpers route through interface conformance instead of struct flags.

### What gets removed

- The `linear` keyword (lexer + grammar rule in 3.8).
- The `@copy` directive (parser + sema validation rule from ADR-0008/ADR-0053).
- The `is_copy`, `is_linear` flags on `StructDef`.
- The dual rule "`@copy` types may not declare `fn drop`" is replaced by the more general "a type may not implement both `Copy` and `Drop`" check, which subsumes it.
- ADR-0008's `@handle` directive (the `is_handle` struct flag and its sema validation; spec §3.8's `@handle` section). Replaced by the convention of writing an inline `fn handle(borrow self) -> Self` (or any other name) — no directive needed since the method is just a method.

### Migration

Phase 5 is a code-mod: every existing `linear struct` becomes a plain `struct`, every `@copy struct` becomes `@derive(Copy)`, and every plain `struct` that the corpus relies on dropping becomes `@derive(Drop)`. The codemod is mechanical:

```
@copy
struct Foo { ... }    →    @derive(Copy) struct Foo { ... }

linear struct Bar { ... }    →    struct Bar { ... }

struct Baz { ... }    →    @derive(Drop) struct Baz { ... }
                            (only if the corpus implicitly drops Baz)
```

We accept that some structs that *were* affine but are never actually dropped become linear — the test suite catches anywhere that drop-on-scope-exit was load-bearing.

### Phase ordering

The new sema sub-phase added by ADR-0058 (derive expansion) already runs before the conformance/ownership classification runs in this ADR. So `@derive(Drop)` splices `fn drop` into the method list, and the subsequent conformance check sees it. No new ordering constraint.

### Preview gate

The whole switch lands behind `--preview ownership_interfaces`. Inside the gate:

- `linear` is rejected with "removed; types are linear by default — see ADR-0059".
- `@copy` is rejected with "removed; use `@derive(Copy)` — see ADR-0059".
- Default ownership for `struct`/`enum` flips from affine to linear.

Outside the gate, current behavior is preserved during migration.

## Implementation Phases

- [ ] **Phase 1: Define `Drop` and `Copy` interfaces in the prelude; recognize them.**
  - Add `Drop` and `Copy` to `KnownSymbols`.
  - Inject the interface declarations during built-in injection.
  - No semantic change yet — just the names exist and resolve.
  - **Testable**: `comptime T: Drop` parses and resolves; an empty body conforms iff it has the right methods.

- [ ] **Phase 2: Wire ownership classification through interface conformance.**
  - Add `is_type_drop(ty)` and `is_type_copy_via_interface(ty)` helpers in `sema/builtins.rs`.
  - Re-implement `is_type_copy` and `is_type_linear` (and `ownership_variant_index`) on top of conformance.
  - Behind `--preview ownership_interfaces`: replace `StructDef::is_copy` / `is_linear` reads with conformance lookups.
  - **Testable**: `@ownership(T)` returns the same value via the new path for every existing test program (under preview).

- [ ] **Phase 3: Synthesize built-in conformance.**
  - During `inject_builtin_types`, attach `Drop`/`Copy` to primitives, pointers, enums, tuples, arrays, and `String` per the table above.
  - **Testable**: `@ownership(i32) == Ownership::Copy`, `@ownership(String) == Ownership::Affine`, with `linear` and `@copy` *removed from those types' definitions*.

- [ ] **Phase 4: Auto-derives `derive Drop` and `derive Copy` in the std prelude.**
  - Write the bodies on top of ADR-0058.
  - Verify the "linear field" / "non-Copy field" diagnostics fire from inside the derive body (no new compiler logic — comes from the comptime-recursive call to `drop`/`copy` failing to resolve).
  - **Testable**: `@derive(Drop) struct S { x: SomeLinear }` emits a multi-span diagnostic citing the derive site and the offending linear field; `@derive(Drop) struct S { x: i32 }` (Copy field) compiles cleanly and produces an affine `S`; `@derive(Copy) struct S { s: String }` (Drop field) emits a diagnostic citing the offending field.

- [ ] **Phase 5: Codemod the corpus.**
  - Replace `linear struct X { ... }` → `struct X { ... }`.
  - Replace `@copy struct X { ... }` → `@derive(Copy) struct X { ... }`.
  - Add `@derive(Drop)` to plain structs that the test corpus relies on dropping (detect by running `make test` and adding the directive on each "linear value dropped" failure).
  - Spec tests for `linear`-specific behavior get reworded to use plain structs; spec tests for `@copy` get reworded to use `@derive(Copy)`.

- [ ] **Phase 6: Remove `linear` keyword and `@copy` directive.**
  - Lexer: remove `linear` keyword.
  - Parser: remove `linear` grammar rule from struct decls; remove `@copy` directive recognition.
  - Sema: remove `StructDef::is_copy` / `is_linear` flags entirely; downstream code reads conformance.
  - Drop the `--preview ownership_interfaces` gate (the new behavior becomes default).

- [ ] **Phase 7: Spec + traceability.**
  - Update §3.8 to describe ownership in terms of interface conformance.
  - Mark §4.13:108–114 (`@ownership`) as defined via Drop/Copy conformance.
  - Mark ADR-0008 as superseded by ADR-0059 (only the `superseded-by` field on ADR-0008 changes).
  - Run traceability check; backfill any uncovered paragraphs.

## Consequences

### Positive

- **One mental model.** Ownership posture *is* interface conformance. No special directives, no special keywords.
- **User-extensible.** A library can now write `fn process(comptime T: Drop, t: T)` (or `fn process(t: Drop)` for runtime dispatch) and rely on the compiler to enforce conformance — same surface as any other interface per ADR-0056. Same for `Copy`, and for any future "trait-like" thing.
- **`@derive(Drop)` does the right thing recursively.** ADR-0058's comptime-walk-fields pattern handles structural drops without any new compiler code.
- **Compile-time errors at the right site.** `@derive(Drop)` on a struct with a linear field fails *at the derive site*, not at the use site — exactly the behavior the user asked for.
- **`@handle` is gone.** Explicit duplication on an affine type (rc-bump, interning, etc.) becomes an ordinary inline method — no directive, no sema validation hook, no `is_handle` struct flag.
- **Three knobs collapse to two interface names**, both of which the user already knows from the language's interface system.

### Negative

- **Default-linear is verbose.** Every struct that wants drop-on-scope-exit must write `@derive(Drop)`. New users will hit "linear value dropped" errors on their first `struct Foo`. We mitigate with a clear error message that suggests adding `@derive(Drop)`.
- **Migration cost.** Phase 5 touches every struct in the test corpus. Mostly mechanical, but tedious.
- **`Drop` and `Copy` are compiler-recognized.** Two interfaces are special. We tolerate this — the language still treats them like ordinary interfaces in every public-facing way; the only special-casing is "the compiler reads conformance to make decisions".

### Neutral

- **`fn drop(self)` is no longer compiler-special.** It's just a method that happens to satisfy the `Drop` interface. ADR-0053's "inline drop" recognition becomes "structural conformance to `Drop`".
- **Interaction with `inout`/exclusivity unchanged.** Ownership posture changes how a value moves; it doesn't change how `inout` borrows behave.

## Open Questions

1. **Dispatching `drop` on a comptime-known field type — likely already supported.** The pattern this ADR needs is `@field(self, f.name).drop()` inside `comptime_unroll for f in @type_info(Self).fields`. The Zig analogue (`@field(self, f.name).deinit()` inside `inline for`) is the canonical recursive-deinit idiom, and Gruel already has the same machinery: postfix method calls apply to any primary expression including intrinsic results (`gruel-parser/src/chumsky_parser.rs:1561-1770`); `@field` returns the field's static type so `.method()` resolves normally (`gruel-air/src/sema/analysis.rs:4486-4498`); `comptime_unroll for` over `@type_info(Self).fields` is implemented (`gruel-air/src/sema/analysis.rs:2449-2499`); and the agent who reviewed this confirmed an end-to-end scratch program that does `@field(val, field.name).doubled()` compiles and runs. The remaining detail is that ADR-0058 type-checks derive bodies once with `Self` free, so `@field(self, f.name)`'s static type is unknown at derive-definition time. Either the derive body is treated like a comptime-generic body (re-checked per expansion) or `Self`-dependent expressions inside derives are deferred to expansion time. Phase 4 picks one. No new intrinsic should be needed.

2. **Empty `@derive(Drop)` body.** `@derive(Drop)` on a struct with all-`Copy` fields produces a valid no-op `drop` and makes the struct affine (= conforms to `Drop`). Note this is *not* the same as the struct being `Copy` — the user explicitly asked for affine semantics by writing `@derive(Drop)` instead of `@derive(Copy)`.

3. **`@derive(Copy)` rejects pre-existing `fn drop`.** Because `Copy` and `Drop` are mutually exclusive, `@derive(Copy)` on a struct that already declares `fn drop(self)` is an error. The diagnostic should cite both the `@derive(Copy)` site and the inline `fn drop` site. Symmetrically, `@derive(Drop)` on a `@derive(Copy)`-marked struct is an error.

4. **Builtin enums (`Arch`, `Os`, `TypeKind`, `Ownership`)** — synthesized as `Copy` per the table. No work needed beyond Phase 3.

5. **Linear field of an `enum` variant.** A linear payload makes the entire enum linear. Already covered by the structural conformance check (enum `Drop` derive body must drop every variant; the comptime walk fails on a linear payload), but document it in §3.8.

## Future Work

- **`Eq`, `Hash`, `Default`, `Clone` derives.** All ride the same ADR-0058 substrate. Each becomes a separate small ADR or just a stdlib addition.
- **Auto-`@derive(Drop)` for "trivially-droppable" structs.** Could revisit option (b) from the design discussion: make a struct with only-Drop fields auto-conform to `Drop` without an explicit `@derive`. Out of scope for this ADR; the explicit form is the floor.

## References

- ADR-0008 — Affine Types and Mutable Value Semantics (the system this supersedes)
- ADR-0010 — Destructors (early Drop story; folded into ADR-0053)
- ADR-0053 — Inline Methods and `fn drop` Recognition
- ADR-0056 — Structural Interfaces
- ADR-0057 — Anonymous Interfaces
- ADR-0058 — User-Defined Derives via `derive` Items
- `@ownership(T)` intrinsic (added in 4dd376c1) — already exposes the trichotomy this ADR is reframing.

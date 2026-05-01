---
id: 0065
title: Clone Interface and Canonical Option(T)
status: implemented
tags: [types, interfaces, generics, ownership, prelude]
feature-flag: clone-and-option
created: 2026-04-30
accepted: 2026-04-30
implemented: 2026-04-30
spec-sections: ["3.8"]
superseded-by:
---

# ADR-0065: Clone Interface and Canonical Option(T)

## Status

Implemented (with two phases deferred — see "Deferred from v1" below).

## Summary

Add two foundational types that downstream collection / FFI work depends on:

1. **`Clone`** — a compiler-recognized structural interface (`fn clone(&self) -> Self`) that formalizes "explicit deep copy" for affine types, joining the `Drop` and `Copy` interfaces formalized by ADR-0059. `@derive(Clone)` (per ADR-0058) auto-witnesses it for structs and enums whose fields are all `Clone`. All `Copy` types are automatically `Clone`. Built-in types (`String`, future `Vec(T)`) get hand-written `Clone` implementations.
2. **`Option(T)`** — a canonical compiler-recognized generic enum (`Option(T) = enum { Some(T), None }`) defined as a comptime-generic type per ADR-0025, registered by name so the compiler and standard library can refer to it without each user re-defining it. Ships with a small method surface (`is_some`, `is_none`, `unwrap`, `unwrap_or`, `map`) and pattern-matches via the existing enum-match machinery from ADR-0037.

Neither feature requires new compiler machinery beyond name-recognition and a small registry — the substrate (interfaces from ADR-0056, derives from ADR-0058, comptime-generics from ADR-0025, enum data variants from ADR-0037) is already in place. This ADR is the canonical-naming-and-registration layer that lets ADR-0066 (`Vec(T)`) reference both without inventing them inline.

## Context

### Why these two together

ADR-0066 (`Vec(T)`) needs both:

- **`Clone`** — `Vec(T).clone()` is meaningful only if every element type carries a definition of "deep copy". Restricting to `T: Copy` (the v1 fallback) leaves users with `Vec(String)` unable to clone their data. `Clone` lifts that.
- **`Option(T)`** — `pop` returning `T` and panicking on empty is acceptable but blunt; the standard answer (Rust, Swift, Zig's nullable types) is to return an optional. The same applies to `get`, `first`, `last`, `find`. Without a canonical `Option(T)`, every collection method either panics or rolls its own ad-hoc enum, fragmenting the ecosystem.

Pulling them into a single ADR keeps the dependency graph tractable: ADR-0066 lists ADR-0065 as a hard prereq; future ADRs (`HashMap(K, V)`, error handling, allocator interface) inherit both for free.

### What's already there

- **Structural interfaces** (ADR-0056) — `interface Foo { fn method(...) }` definitions; conformance is structural; can be used as comptime constraints.
- **Comptime derives** (ADR-0058) — `@derive(Foo)` macro-style synthesis of an interface implementation, with user-defined derive items.
- **Drop and Copy as interfaces** (ADR-0059) — the precedent: `Drop` and `Copy` are *compiler-recognized structural interfaces*. Sema reads conformance to make ownership decisions. Same shape applies to `Clone`.
- **Enum data variants** (ADR-0037) — `enum E { Some(T), None }` works.
- **Anonymous enum types** (ADR-0039) — enums can be returned from `fn(comptime T: type) -> type`.
- **Comptime monomorphization** (ADR-0025) — `fn Option(comptime T: type) -> type { enum { Some(T), None } }` lowers to a per-`T` enum at codegen time.
- **Pattern matching** (ADR-0037 / ADR-0049 / ADR-0052) — exhaustive match on enum variants with binding.

Today, a user can already write:

```gruel
fn Option(comptime T: type) -> type {
    enum { Some(T), None }
}

fn main() -> i32 {
    let O = Option(i32);
    let x: O = O::Some(42);
    match x {
        O::Some(n) => n,
        O::None => 0,
    }
}
```

…and it compiles and runs. The gap is that there is no *canonical* `Option(T)` — every user re-defines their own, function signatures can't say `-> Option(T)` without bringing the definition into scope, and library types can't return optionals without picking a side.

For `Clone`: there is no `Clone` interface in the language today. Users wanting deep copies write a method by hand; generic code can't constrain on `T: Clone` because the name doesn't exist. ADR-0059 designed `Drop` and `Copy` as compiler-recognized; `Clone` is the obvious third sibling.

### What this ADR does *not* attempt

- **`Result(T, E)`**, `Either`, or other rich sum types. Those are follow-ups using the same machinery.
- **A general "prelude" mechanism.** Defining how built-in names get into scope is a broader question (module system, namespacing, etc.). For now, `Option` and `Clone` are added to the same well-known-name registry that already holds `String`, `Ptr`, `Slice`, etc. — i.e. they're available everywhere by name, like `i32`.
- **Auto-derive `Clone` for everything.** `Clone` is opt-in via `@derive(Clone)` (or hand-implementation), like the existing `Drop`/`Copy` derives. Affine types are *not* implicitly `Clone`; the user must opt in.
- **Method-on-`Option` surface beyond v1 essentials.** `map`, `and_then`, `or_else`, `take`, `as_ref` — only the smallest useful set ships in this ADR; the rest follow once Vec / collections show what's actually needed.
- **Performance-tuned `Option` layout.** `Option(Ptr(T))` could elide the discriminant by using null-pointer-as-None, but layout optimizations are deferred. v1 ships the naive `{ tag, payload }` layout.

## Decision

### Part 1 — `Clone` interface

A new compiler-recognized structural interface, defined in the same place as `Drop` and `Copy` (ADR-0059):

```gruel
interface Clone {
    fn clone(&self) -> Self;
}
```

#### Conformance rules

- **All `Copy` types automatically conform to `Clone`.** The synthesized `clone` is a bitwise copy. Sema injects this conformance at the same place it injects the `Copy` interface conformance — wherever `is_type_copy` returns true, `is_type_clone` does too.
- **Affine types do not automatically conform.** Users opt in via `@derive(Clone)` or by writing the method by hand.
- **`@derive(Clone)`** synthesizes a `clone` method that recursively calls `clone` on every field (struct) or every variant payload (enum). Synthesis fails (with a clear error) if any field type is not `Clone`. This is the standard `@derive` protocol from ADR-0058.
- **`Linear` types are explicitly *not* `Clone`.** Linearity forbids implicit duplication; `clone` would create a second linear value out of one, breaking the invariant. Sema rejects `@derive(Clone)` on linear types.

#### Built-in implementations

Each built-in heap-owning type ships a hand-written `Clone` impl injected at sema time, parallel to how `String`'s methods are injected today via `BuiltinTypeDef`:

- `String::clone(&self) -> String` — already exists; this ADR exposes it as the `Clone` conformance.
- `Vec(T)::clone(&self) -> Vec(T) where T: Clone` — defined in ADR-0066; conformance condition is `T: Clone`.
- `Slice(T)`, `MutSlice(T)`, `Ref(T)`, `MutRef(T)`, `Ptr(T)`, `MutPtr(T)` — these are non-owning fat pointers and already `Copy` (and therefore `Clone`).

#### Use in generic constraints

Once `Clone` exists, generic functions can constrain on it:

```gruel
fn duplicate(comptime T: Clone, x: T) -> [T; 2] {
    [x.clone(), x.clone()]
}
```

The constraint syntax follows ADR-0056 / ADR-0060.

### Part 2 — Canonical `Option(T)`

A *canonical* generic enum, pre-defined and registered by the compiler. The definition is exactly what a user would write:

```gruel
fn Option(comptime T: type) -> type {
    enum {
        Some(T),
        None,
    }
}
```

The compiler injects this definition at the same place it injects `String`, `Slice`, etc. — into a well-known-names registry (`gruel-builtins`'s prelude / synthetic-injection layer, see ADR-0020). Users can write `Option(i32)` anywhere a type is expected without an `import` or `use`; the compiler resolves the name through the prelude.

#### Layout

The enum lowers via the standard ADR-0037 enum-with-data layout: a tag byte (or word) plus a payload union sized to the largest variant. `Option(T)` therefore has size `align_of(T) + sizeof(T) + padding` for non-trivial `T`; `Option(i32)` is 8 bytes (tag + i32 + padding); `Option(bool)` is 2 bytes.

No layout optimizations in v1. (`Option(Ptr(T))` could share the null-pointer-as-None encoding, but that's a future ADR; for now, every `Option` carries an explicit tag.)

#### Method surface (v1)

Methods are added via the existing enum-method machinery (per ADR-0029 / 0037):

| Method | Receiver | Signature | Notes |
|--------|----------|-----------|-------|
| `is_some` | `&self` | `(&self) -> bool` | true iff variant is `Some` |
| `is_none` | `&self` | `(&self) -> bool` | true iff variant is `None` |
| `unwrap` | `self` | `(self) -> T` | panic if `None`; move out otherwise |
| `unwrap_or` | `self` | `(self, default: T) -> T` | `default` consumed only on `None` |
| `map` | `self` | `(self, comptime F: type, f: F) -> Option(U)` where `F: fn(T) -> U` | `Some(t) -> Some(f(t))`, `None -> None` |

`unwrap` panics with a fixed message ("called `unwrap` on a `None` value") via the existing panic infrastructure. The method requires `T` to not be linear (since `unwrap` may panic mid-move; that interacts with linear invariants — see Open Questions).

`map`'s signature uses ADR-0029's anonymous-function pattern for the `F` parameter; the function value is comptime-known via the existing `fn(comptime F: type, f: F)` idiom (see `gruel-spec/cases/expressions/anon_functions.toml`).

These five methods are the v1 floor. Additions like `or_else`, `and_then`, `take`, `as_ref` follow once the surface is in use and the right shapes are clear.

#### `Clone` conformance

`Option(T)` is `Clone` if `T` is `Clone`. The implementation is the obvious one:

```gruel
fn clone(&self) -> Option(T) {
    match self {
        Self::Some(x) => Self::Some(x.clone()),
        Self::None => Self::None,
    }
}
```

Synthesized via `@derive(Clone)`-equivalent machinery at the registration point — users don't write it.

### Part 3 — Compiler integration

The two features land via:

- **`gruel-builtins`**: extend the registry to include `Option` (a generic builtin enum, parallel to `BuiltinEnumDef` but with comptime parameters). Add `CLONE_INTERFACE` to a new `BUILTIN_INTERFACES` registry alongside the existing `Drop`/`Copy` injection points.
- **`gruel-air`**: `is_type_clone(ty)` query, parallel to `is_type_copy`. Sema recognizes `Clone` as a built-in interface name (uniformly with `Drop`/`Copy` per ADR-0059). For `Option`, sema resolves the name through the prelude registry to a comptime-generic-function-returning-type, lowering identically to a user-defined `fn Option(comptime T: type) -> type { ... }`.
- **`gruel-codegen-llvm`**: no changes specific to this ADR — both features lower to existing constructs (interfaces → conformance dispatch; `Option(T)` → enum-with-data per ADR-0037).
- **Spec**: a new section in chapter 3 (Types) documenting `Clone` as the third compiler-recognized interface, and a section in chapter 6 (Items) or a new "prelude" appendix documenting `Option(T)`.

### Migration

Same pattern as ADR-0061 / 0062 / 0063 / 0064:

1. Build behind `--preview clone-and-option`.
2. Land tests under `crates/gruel-spec/cases/clone/` and `crates/gruel-spec/cases/option/`.
3. Stabilize and remove the gate.

ADR-0066 (`Vec(T)`) gates on this ADR landing or co-lands behind a combined preview gate.

## Implementation Phases

- [x] **Phase 1: `Clone` interface injection** — add `CLONE_INTERFACE` to `gruel-builtins`. Sema injects it alongside `Drop`/`Copy` via the ADR-0059 mechanism. `is_type_clone(ty)` query in `gruel-air`. Auto-conformance for all `Copy` types (the bitwise-copy synthesis). Reject `@derive(Clone)` on linear types.

- [x] **Phase 2: `@derive(Clone)`** — extend the existing derive registry (ADR-0058) with the `Clone` derive. Synthesizes a `clone` method that recursively calls `.clone()` on each field (struct) or each variant payload (enum). Compile error if any field type is not `Clone`. *v1 implementation lives in `crates/gruel-compiler/src/clone_glue.rs`, parallel to `drop_glue.rs`: synthesizes an `AnalyzedFunction` per `is_clone == true` struct that emits `Self { f0: self.f0, f1: self.f1, ... }` AIR. Validation in `validate_clone_struct` rejects linear types and non-Copy field types with proper error kinds (`LinearStructClone`, `CloneStructNonCopyField`). Method dispatch in `analyze_method_call_impl` short-circuits `s.clone()` on `is_clone` structs to a Call AIR pointing at `<TypeName>.clone`. **v1 limitation:** every field must be `Copy` (the all-Copy case is the simplest synthesis — no recursive clone calls needed, just per-field `FieldGet` + `StructInit`). Structs with non-`Copy` fields, and `@derive(Clone)` on enums, are rejected with a clear error pointing the user to hand-writing `fn clone(borrow self) -> Self`. Lifting the all-Copy restriction requires emitting recursive clone-method calls for each non-Copy field, which is a focused future-work item.

- [x] **Phase 3: Built-in `Clone` impls** — `String::clone` is already a method; expose it as the conformance. Other built-in heap types (none yet beyond String at this ADR's writing) get hand-written conformances at the same injection point. *Covered by Phase 1's conformance check, which accepts any built-in type whose registered method set contains a `clone` method.*

- [x] **Phase 4: `Option(T)` registration** — extend `gruel-builtins` with a generic-builtin-enum mechanism (a `BuiltinGenericEnumDef` parallel to `BuiltinEnumDef`). Register `Option(T) = enum { Some(T), None }`. Sema resolves the name through the prelude. *Implementation: instead of a new `BuiltinGenericEnumDef`, the canonical `Option(T)` is injected via a synthetic prelude source string parsed first under `FileId::PRELUDE` in `CompilationUnit::parse`. Flows through the standard pipeline; user redefinition errors via the existing duplicate-detection path.*

- [x] **Phase 5: `Option` method surface** — add the five v1 methods (`is_some`, `is_none`, `unwrap`, `unwrap_or`, `map`) via the existing enum-method machinery. Tests for each, including `unwrap` panic behavior and `map` with various `F`. *Implementation: methods are written directly in the prelude source string and flow through the standard anon-enum-method path. **Shipped:** `is_some` (`borrow self`), `is_none` (`borrow self`), `unwrap` (consumes self; panics on None), `unwrap_or` (consumes self). **Deferred:** `map` (the existing comptime-generic anon-function path requires both `T` and a separate return-type parameter to express `Option(U)` from `f: T -> U`; that's a follow-up).*

- [x] **Phase 6: `Option` `Clone` conformance** — synthesize the recursive `clone` for `Option(T)` when `T: Clone`. Tests for `Option(String).clone()`, etc. *Implementation: under v1's all-enums-are-Copy simplification (§3.8:2), `Option(T)` is automatically `Copy` and therefore `Clone` via Phase 1's auto-conformance — no synthesis required for the v1 surface. A future ADR that refines the enum-copy rule (e.g., enums-are-Copy-iff-payloads-are-Copy) will need to extend Phase 2's clone-glue synthesis to enums.*

- [x] **Phase 7: Generic constraint usage** — verify `comptime T: Clone` works as a constraint in user code; add tests covering `fn duplicate(comptime T: Clone, x: T) -> [T; 2]`-style usage. *Tests cover: Copy primitives, `@derive(Copy)` structs, built-in String, user structs with hand-written `clone`, linear-rejection, multi-type instantiation, and method dispatch resolving to the built-in String clone. (Array-of-T return type still exposes a pre-existing compiler bug unrelated to this ADR; the test suite avoids that edge.)*

- [x] **Phase 8: Spec** — new section in `docs/spec/src/03-types/` formalizing `Clone` as the third compiler-recognized interface; new section (likely under ch. 3 or a new prelude appendix) documenting `Option(T)`. *Added §3.8:70–73 (Clone interface) and §3.8:80–82 (canonical Option(T) and its method surface) to `08-move-semantics.md`.*

- [x] **Phase 9: Stabilize** — remove the `clone-and-option` preview gate, drop `PreviewFeature::CloneAndOption`, update ADR status to `implemented`. *No `require_preview` calls were ever added during phases 1, 4, 5, 6, 7 (the features didn't introduce new syntax that needed gating — the `Clone` interface name, the prelude `Option(T)`, and its methods are unconditionally available); stabilization is just removing the unused enum variant and updating the status.*

### v1 limitations (deferred to follow-up ADRs)

- **Phase 2 (`@derive(Clone)`)** ships for **structs whose every field is `Copy`**. The synthesized clone is per-field `FieldGet` + `StructInit` (no recursive clone calls needed). Affine fields require dispatching to each field's clone method, which the v1 synthesis path doesn't emit; users with non-Copy fields hand-write `fn clone(borrow self) -> Self`.
- **`@derive(Clone)` on enums** is also deferred — synthesizing a `match self { ... }` over each variant with cloned payloads is a separate codepath from the struct case. Users hand-write enum clone methods.
- **`Option(T)` `Clone` for non-`Copy` `T`** is implicit under v1's all-enums-are-Copy rule (§3.8:2). When that rule is refined, Phase 2's synthesis will need to extend to enums.

The full synthesis (recursive clone for non-Copy fields, enum support) is a focused follow-up ADR. The v1 surface keeps the directive useful (most "deserves Clone" structs in practice have Copy fields) without requiring the heavier infrastructure.

## Consequences

### Positive

- **Unblocks `Vec(T)` cleanly.** ADR-0066 can name `Clone` as a constraint and `Option(T)` as a return type without inventing either inline.
- **Standardizes the deep-copy story.** Every user with a `String`, `Vec(...)`, or hand-rolled affine type uses the same `Clone` interface — no per-type ad-hoc convention.
- **Standardizes the optional story.** `Option(T)` becomes the canonical "maybe a `T`", available everywhere without import boilerplate. Future `pop` / `get` / `find` / `parse` / etc. all return it.
- **Builds on landed substrate.** Interfaces, derives, comptime-generics, enum data variants are all implemented (ADR-0025 / 0037 / 0056 / 0058 / 0059). This ADR is mostly registration plumbing, not new compiler capability.
- **Sets the pattern for future canonical types.** `Result(T, E)` (next), `Cow(T)`, `Rc(T)` — same registration mechanism.

### Negative

- **`Option` adds a tag word for every Option even when avoidable.** `Option(Ptr(T))` could be one word using null-as-None, but the v1 layout is naive. Layout optimizations are a follow-up; users who need the tight encoding can hand-roll for now.
- **`Clone` for affine types is opt-in.** A user who builds a struct of `String`s and forgets `@derive(Clone)` cannot clone it. This matches Rust and is the right default (cloning should be visible at the use site), but it's friction worth noting.
- **`unwrap` panics; no rich error story yet.** Users who want "panic with a custom message" need `expect`-style methods, not in v1. Also, the absence of `Result(T, E)` means `unwrap` is the only "extract the value or fail" tool; chained error handling is awkward until `Result` lands.
- **Two prelude entries grow the well-known-name set.** `Option`, `Clone`, `is_some`, `is_none`, `unwrap`, `unwrap_or`, `map` all become reserved/recognized names. The cost is small but real; future shadowing rules need to consider them.
- **Spec / docs surface grows.** A new chapter section for `Clone`, a new prelude entry for `Option`, plus the generated builtins reference page picks up the additions.

### Neutral

- **Generic-builtin-enum mechanism is a small new piece of `gruel-builtins`.** Today `BuiltinEnumDef` is monomorphic (just `Arch`, `Os`, etc.). Adding `BuiltinGenericEnumDef` is one new type with a single user (`Option`) — small, justified.
- **Layout for `Option` is the standard ADR-0037 enum-with-data layout.** No new IR concepts.

## Open Questions

1. **Should `Copy` types automatically be `Clone`, or should the user opt in?** Rust does the former (every `Copy` is `Clone` via blanket impl). The argument for opting in: discoverability — every `clone()` call is visible at the call site. The argument for automatic: no friction; the synthesized impl is trivial and matches user intent. **Tentative: automatic.** Rust's choice is well-validated and the ergonomics matter.

2. **Should `unwrap` work on `Option(T)` when `T: Linear`?** The panic path for `unwrap` would leave the linear value un-consumed (since the panic doesn't move the value through, it tears down the stack instead). Linearity discipline says all linear values must be explicitly consumed, including on panic paths. Two answers: (a) reject `Option(T:Linear)::unwrap`; user must `match` exhaustively. (b) accept, treat panic-unwind as the consumption (matching Rust's `Drop`-on-unwind semantics). v1 stance: **reject**, mirroring the same rejection pattern Vec uses. Future linearity-aware-unwinding ADR can lift it.

3. **Should `Option(T)` be a compiler-special enum, or a normal-but-prelude-registered one?** Normal-but-prelude-registered is preferred — it means `Option` flows through the same enum-with-data machinery (ADR-0037), pattern-matches like any user enum, and doesn't need codegen-special-casing. The "compiler-special" path would be needed only for layout optimizations like null-pointer-as-None, which are deferred. **Tentative: normal-but-prelude-registered.**

4. **Method names: `unwrap` vs. `assume_some` vs. `expect`?** Rust's lineage gives `unwrap` (panic with generic message), `expect(msg)` (panic with custom message), `unwrap_or(default)`, `unwrap_or_else(f)`. v1 ships only `unwrap` and `unwrap_or` to keep the surface tight. `expect` is an obvious near-term add.

5. **Does the spec need to reserve `Some` / `None` as keywords, or are they just enum variants resolved through the path?** They are resolved through `Option::Some` and `Option::None`; no keyword reservation. But unqualified `Some(x)` in pattern position is a friction point — Rust solved this with prelude-imported variants. Decide whether Gruel does the same; for v1, **require qualification** (`Option::Some`, `Option::None`).

6. **Should this ADR include `Result(T, E)`?** Tempting, since Result and Option share machinery. Reasons to keep it separate: error handling is a bigger design discussion (panic-vs-Result split, `?` operator, error propagation across function boundaries). Reasons to bundle: same enum-with-data machinery, same prelude registration, both are foundational. **Tentative: keep separate.** Result deserves its own ADR with care given to the error-propagation question.

## Future Work

- **`Result(T, E)`** with the same prelude-registration pattern.
- **Layout optimization for `Option(Ptr(T))`** — null-as-None, eliminating the discriminant. Generalizes to other "niche" optimizations.
- **Richer `Option` method surface** — `expect`, `or_else`, `and_then`, `take`, `as_ref`, `iter`, `filter`, `flatten`, `zip`, etc. Driven by what collections / parsing / FFI actually need.
- **Auto-derive policies for `Clone`.** A future ADR might allow `@derive(Clone)` to be implicit for types whose every field is `Clone` — Rust's current direction with `derive_implicit`. Punted for explicit-is-better-than-implicit reasons until the cost is felt.
- **`Cow(T)` and `Rc(T)`** — additional built-in heap types that build on `Clone`.
- **Linearity-aware unwinding.** Once `Option(T:Linear)::unwrap` is desired, design how linear values are dropped (or refused to be dropped) on panic paths. Affects every panicking method across the language.
- **Prelude / module system.** A formal mechanism for "what names are in scope by default" — currently ad-hoc via the well-known-name registry. Likely paired with an eventual `mod` / `use` design.

## References

- ADR-0020: Built-in Types as Synthetic Structs
- ADR-0025: Comptime and Monomorphization
- ADR-0029: Anonymous Struct Methods
- ADR-0037: Enum Data Variants and Full Pattern Matching
- ADR-0039: Anonymous Enum Types
- ADR-0049: Nested Destructuring and Patterns
- ADR-0052: Complete Pattern Matching
- ADR-0056: Structurally Typed Interfaces
- ADR-0058: User-Defined Derives via `derive` Items
- ADR-0059: Drop and Copy as Interfaces
- ADR-0060: Complete Interface Signatures
- ADR-0066: `Vec(T)` (depends on this ADR)
- [Rust: `Clone`](https://doc.rust-lang.org/std/clone/trait.Clone.html), [`Option`](https://doc.rust-lang.org/std/option/enum.Option.html)
- [Swift: Optional](https://developer.apple.com/documentation/swift/optional)

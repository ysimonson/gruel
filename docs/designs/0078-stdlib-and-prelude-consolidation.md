---
id: 0078
title: Stdlib MVP — Prelude on Disk, Built-in Declarations to Gruel, and Eq/Ord Operator Interfaces
status: proposal
tags: [stdlib, prelude, builtins, interfaces, operators]
feature-flag: stdlib_mvp
created: 2026-05-03
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0078: Stdlib MVP

## Status

Proposal

## Summary

Establish the first non-trivial layer of the Gruel standard library by making four changes that, together, turn `std/` from a four-function math stub into a real home for stdlib code:

1. **Prelude as `std/prelude/` directory.** Move the inline `PRELUDE_SOURCE` raw-string constant in `crates/gruel-compiler/src/unit.rs:90-316` (~225 lines of Gruel embedded in Rust) onto disk under `std/prelude/`, with a per-topic file split (`option.gruel`, `result.gruel`, `char.gruel`, `string.gruel`, `interfaces.gruel`, `target.gruel`, `cmp.gruel`). Names declared in any prelude file are auto-imported into user code without an `@import` — a small "prelude scope flattening" mechanism is added to support that.
2. **Built-in interface declarations move to Gruel.** `Drop`, `Copy`, `Clone`, `Handle` (currently `BuiltinInterfaceDef` records in `gruel-builtins/src/lib.rs:945-1015`) become `pub interface` declarations in `std/prelude/interfaces.gruel`. The compiler keeps its hardcoded behavior (drop glue at scope end, `@derive(Copy)` validation, `@derive(Clone)` synthesis, `Handle` linearity carve-out) keyed off interned names — same pattern as how it recognizes `Option` and `Result` today.
3. **Built-in enum declarations move to Gruel.** `Arch`, `Os`, `TypeKind`, `Ownership` (in `gruel-builtins/src/lib.rs:657-717`) become `pub enum` declarations in `std/prelude/target.gruel`. The intrinsics that produce values of these types (`@target_arch`, `@target_os`, `@type_info`, `@ownership`) switch from variant-by-index lookup to variant-by-name lookup against the Gruel-defined enum.
4. **Eq, Ord, and operator desugaring.** Add `pub interface Eq`, `pub interface Ord`, and `pub enum Ordering` to `std/prelude/cmp.gruel`. Sema's binary-operator analyzer gains a fall-through path: when operands are not built-in primitives, look up `Eq` / `Ord` conformance and desugar `==`, `!=`, `<`, `<=`, `>`, `>=` to method calls on `eq` / `cmp`. This is real operator overloading — the language gains it as a side effect of moving stdlib types out of Rust.

**Explicitly deferred to a follow-up ADR.** This ADR does **not** touch `String` or `Vec(T)`. ADR-0072's runtime-collapse target (~750 LOC of `gruel-runtime/src/string.rs` shrinking to a UTF-8-validation core) is real and tracked, but landing it cleanly depends on (a) operator overloading existing for non-built-in types, and (b) the prelude being a directory module rather than a string. This ADR delivers (a) and (b); a sibling ADR consumes them.

**LOC impact.** ~250 Rust LOC removed (~415 deletions, ~165 additions for new operator-dispatch logic and the prelude loader), ~285 Gruel LOC added. The win is modest by line count; the structural value is **the language gains operator overloading and the stdlib gets a real foundation**, both of which unlock the larger `String`/`Vec` collapse later.

## Context

### Where things sit today

**Stdlib.** `std/_std.gruel` (re-exports `math`) and `std/math.gruel` (`abs`, `min`, `max`, `clamp`) — 30 LOC of Gruel total, plus the resolution machinery in `crates/gruel-air/src/sema/analysis.rs:5620` (`resolve_std_import`). `@import("std")` works; `GRUEL_STD_PATH` and `std/`-relative search are wired.

**Prelude (the inline string).** `crates/gruel-compiler/src/unit.rs:90` defines `const PRELUDE_SOURCE: &str = r#"…"#;` — 225 lines containing `Option(T)`, `Result(T, E)`, `char__from_u32`, `char__is_ascii`, `char__len_utf8`, `char__encode_utf8`, `Utf8DecodeError`, `String__from_utf8`, `String__from_c_str`. Loaded under `FileId::PRELUDE` before user code (`unit.rs:526`); names are visible without `@import`. The mechanism that makes them globally visible is **not** the standard module re-export pattern — `FileId::PRELUDE`'s top-level items go straight into the global resolution table.

**Built-in interfaces.** `gruel-builtins/src/lib.rs:945-1015`, registered into `BUILTIN_INTERFACES`:

- `Drop` (ADR-0059) — method-presence conformance; `fn drop(self)` makes a type conform.
- `Copy` (ADR-0059) — `@derive(Copy)` only; compiler emits a bitwise copy; method body is never user-written.
- `Clone` (ADR-0065) — `@derive(Clone)` synthesizes a recursive clone; rejected on linear types.
- `Handle` (ADR-0075) — method-presence conformance; permitted on linear types.

Sema injects them at `crates/gruel-air/src/sema/builtins.rs:145` (`inject_builtin_interfaces`). The *declarations* are pure data; the compiler's behavior is hardwired by name — `Option` and `Result` follow the same recognize-by-name pattern, but their declarations are already in the prelude, so the precedent for moving the declarations exists.

**Built-in enums.** `gruel-builtins/src/lib.rs:657-717`, registered into `BUILTIN_ENUMS`:

- `Arch` (8 unit variants) used by `@target_arch()`
- `Os` (5 unit variants) used by `@target_os()`
- `TypeKind` (7 unit variants) used by `@type_info()`
- `Ownership` (3 unit variants) used by `@ownership(T)`

The intrinsics today materialize values by selecting variant by *index* against `BuiltinEnumDef::variants`. Sema stores special IDs (`builtin_arch_id` etc.) on `Sema` for fast lookup.

**Operators on user types.** Currently impossible. There is no `Eq` or `Ord` interface in the language. The only types with overloaded operators are:

- Numeric primitives (`i32 + i32`, etc.) — direct sema analysis, no dispatch.
- `bool == bool` — direct sema analysis.
- `String` — registry-driven via `BuiltinTypeDef::operators` (`gruel-builtins/src/lib.rs:345-376`), routing to runtime `__gruel_str_eq` / `__gruel_str_cmp`.

A user-defined struct cannot overload `==` today; users must call an explicit method. This is the largest missing piece in the language for ergonomic stdlib types — and the load-bearing reason this ADR exists rather than just relocating registry data.

### Why now

- The on-disk stdlib mechanism is stable (ADR-0026 stable since 2026-01-04) but underused: `std/math.gruel` is the only customer.
- `Result(T, E)` (ADR-0070) and `char` (ADR-0071) prove the "Gruel-resident generic types in prelude" pattern works.
- The inline `PRELUDE_SOURCE` is approaching the size where it resists edits (no syntax highlighting, awkward escaping).
- Operator overloading has been blocked behind "we'll figure it out when we need it." This ADR needs it for the stdlib types it adds, so we figure it out now — minimally, just `Eq` and `Ord`.
- The bigger `String`/`Vec` migration (ADR-0072 anticipates ~490 LOC of `string.rs` retiring) is gated on operator overloading existing for non-built-in types. Shipping this ADR clears the path.

### What this ADR does **not** do

- **Does not move `String` methods to Gruel.** The 31 `no_mangle` extern functions in `gruel-runtime/src/string.rs` (751 LOC) all stay. The `STRING_TYPE` registry entry stays. `String`'s 6 registry-driven operator entries stay (and continue to win over the new Eq/Ord dispatch — see Decision §4).
- **Does not move `Vec(T)` to Gruel.** `Vec(T)` stays as `BuiltinTypeConstructorKind::Vec` with codegen-inlined methods.
- **Does not retire `__gruel_str_eq` / `__gruel_str_cmp`.** They keep being called via the existing `BUILTIN_TYPES` operator-routing path.
- **Does not add `PartialEq`/`PartialOrd`.** Floats keep their primitive comparisons; the Eq/Ord interfaces are for non-float types only (see §4).
- **Does not add interface bounds on generics.** Gruel's comptime generics are structural/duck-typed; once a method exists, monomorphization picks it up. Adding `T: Ord` syntax is a separate ADR.

These are real follow-ups, not shrugs. The next ADR (call it 0079) will consume what this one ships.

## Decision

Four shifts, executed as separate phases.

### Shift 1: Prelude as `std/prelude/` directory module

Replace the inline `PRELUDE_SOURCE` string with a real on-disk tree under `std/prelude/`, loaded automatically before user code.

**Layout.**

```
std/
  _std.gruel              # existing
  math.gruel              # existing
  _prelude.gruel          # NEW — manifest listing prelude submodules
  prelude/
    option.gruel          # Option(T)
    result.gruel          # Result(T, E)
    char.gruel            # char__from_u32, char__is_ascii, char__len_utf8, char__encode_utf8
    string.gruel          # Utf8DecodeError, String__from_utf8, String__from_c_str
    interfaces.gruel      # Drop, Copy, Clone, Handle (Shift 2)
    target.gruel          # Arch, Os, TypeKind, Ownership (Shift 3)
    cmp.gruel             # Eq, Ord, Ordering (Shift 4)
```

**Auto-import via prelude-scope flattening.** The standard `@import("std")` resolution returns a struct; you'd write `prelude.option.Some`. The prelude needs unqualified names. **This is a new behavior**, not a relocation — the current inline prelude works because all its declarations are top-level under one synthetic `FileId::PRELUDE`.

The cheap way to preserve this: when the loader walks `std/prelude/`, every `.gruel` file there is parsed and its top-level `pub` items are merged into a single virtual prelude scope under `FileId::PRELUDE` (or a small range of prelude-flagged ids). `_prelude.gruel` is a manifest — either a literal list of files (`pub const _ = @include_prelude("option.gruel"); ...`) or implicit (every `.gruel` file in `prelude/` is included). Implicit-by-discovery is simpler; pick that unless ordering issues surface.

**Loading.** `CompilationUnit::parse()` (`crates/gruel-compiler/src/unit.rs:523-541`) currently constructs the prelude as `SourceFile::new("<prelude>", PRELUDE_SOURCE, FileId::PRELUDE)`. Replace with: locate `std/prelude/` via the same `GRUEL_STD_PATH` / relative-`std/` machinery `resolve_std_import` uses, parse each `.gruel` file under it as `FileId::PRELUDE`, and prepend the merged AST to the user files.

**Fallback.** Keep a `PRELUDE_FALLBACK` map in Rust mirroring the on-disk files via `include_str!` for tests, missing-stdlib hosts, and binary distribution. The disk is the source of truth; the embedded copy is a safety net.

**FileId discipline.** Either reuse `FileId::PRELUDE` for every prelude file, or add an `is_prelude(file_id)` predicate. The choice affects ADR-0073's privileged-access carve-out — pick whichever keeps that one-liner unchanged.

### Shift 2: Built-in interface declarations → Gruel

Move `Drop`, `Copy`, `Clone`, `Handle` declarations into `std/prelude/interfaces.gruel`:

```gruel
pub interface Drop {
    fn drop(self);
}

pub interface Copy {
    fn copy(borrow self) -> Self;
}

pub interface Clone {
    fn clone(borrow self) -> Self;
}

pub interface Handle {
    fn handle(borrow self) -> Self;
}
```

(Surface syntax to verify against ADR-0056 during Phase 2; if the keyword is `iface` or the receiver-mode marker differs, adjust verbatim.)

**Compiler changes.** Sema looks up the four interfaces by interned name from the prelude scope rather than from `BUILTIN_INTERFACES`. The hardcoded behavior — drop glue at scope end (ADR-0010), `@derive(Copy)` field-by-field validation, `@derive(Clone)` recursive-clone synthesis, `Handle` linearity carve-out (ADR-0075) — stays in Rust, keyed off the interface name.

**Deletions.** `BUILTIN_INTERFACES`, `DROP_INTERFACE`, `COPY_INTERFACE`, `CLONE_INTERFACE`, `HANDLE_INTERFACE`, `BuiltinInterfaceDef`, `BuiltinInterfaceMethod`, `BuiltinIfaceTy`, `BuiltinInterfaceConformance` (~80 LOC), plus `inject_builtin_interfaces` at `crates/gruel-air/src/sema/builtins.rs:145` (~30 LOC).

### Shift 3: Built-in enum declarations → Gruel

Move `Arch`, `Os`, `TypeKind`, `Ownership` into `std/prelude/target.gruel`:

```gruel
pub enum Arch { X86_64, Aarch64, X86, Arm, Riscv32, Riscv64, Wasm32, Wasm64 }
pub enum Os { Linux, Macos, Windows, Freestanding, Wasi }
pub enum TypeKind { Struct, Enum, Int, Bool, Unit, Never, Array }
pub enum Ownership { Copy, Affine, Linear }
```

**Intrinsic adjustment.** `@target_arch`, `@target_os`, `@type_info`, `@ownership` switch from variant-by-index lookup against `BuiltinEnumDef::variants` to variant-by-name lookup against the Gruel-defined enum interned in the prelude. The variant-name → variant-index mapping is computed once at type interning. Side benefit: variants can be reordered in the Gruel source without breaking intrinsic codegen.

**Deletions.** `BUILTIN_ENUMS`, `ARCH_ENUM`, `OS_ENUM`, `TYPEKIND_ENUM`, `OWNERSHIP_ENUM`, `BuiltinEnumDef` (~80 LOC), plus the `builtin_arch_id` / `builtin_os_id` / `builtin_typekind_id` / `builtin_ownership_id` fields on `Sema` and the corresponding injection loop.

### Shift 4: `Eq`, `Ord`, and operator desugaring

Add to `std/prelude/cmp.gruel`:

```gruel
pub enum Ordering { Less, Equal, Greater }

pub interface Eq {
    fn eq(borrow self, borrow other: Self) -> bool;
}

pub interface Ord {
    fn cmp(borrow self, borrow other: Self) -> Ordering;
}
```

**Compiler changes (the load-bearing piece).** The binary-operator analyzer in sema gains a fall-through path:

```
Given `a OP b` where OP ∈ { ==, !=, <, <=, >, >= }:
  1. If both operands are built-in numeric primitives:
       use the existing primitive-op path. (Unchanged.)
  2. Else if both operands are bool and OP ∈ {==, !=}:
       use the existing primitive-op path. (Unchanged.)
  3. Else if `typeof(a)` is in BUILTIN_TYPES and has a registry operator entry:
       use the registry path. (Unchanged — String keeps working.)
  4. Else if OP ∈ {==, !=} and typeof(a) conforms to Eq:
       desugar to `a.eq(other: b)` (and `!` for !=).
  5. Else if OP ∈ {<, <=, >, >=} and typeof(a) conforms to Ord:
       desugar to `match a.cmp(other: b) { … }` against Ordering variants.
  6. Else: type error.
```

Steps 4–5 are new. Steps 1–3 are the existing analyzer untouched.

`Eq` / `Ord` recognition is by interned name from the prelude — same recognize-by-name pattern as `Drop` / `Copy` / `Clone` / `Handle` / `Option` / `Result`. Conformance is structural per ADR-0056: a type conforms to `Eq` if it has a method `fn eq(borrow self, borrow other: Self) -> bool`.

**Float disposition.** `f32` and `f64` keep primitive `==` / `!=` / `<` / etc. via step 1. They do **not** conform to `Eq` or `Ord` automatically — adding partiality (NaN handling) is `PartialEq` / `PartialOrd` territory and out of scope. If a user wants to put a float in a generic slot that requires `Ord`, they'll get a clear "f64 doesn't implement Ord — use a wrapper or write a partial-comparison function" error.

**Existing `String` operators are unaffected.** Step 3 wins before step 4 ever runs. `String`'s 6 registry-driven operator entries keep routing to `__gruel_str_eq` / `__gruel_str_cmp`. Future ADR can give `String` `eq` / `cmp` methods, drop the registry entries, and let it fall through to step 4.

**Comptime monomorphization.** Gruel's comptime generics are structural — a body like `fn max(comptime T: type, a: T, b: T) -> T { if a < b { b } else { a } }` typechecks at instantiation if `<` resolves for `T`. Once `<` desugars through `Ord::cmp`, `T` needs to provide a `cmp` method. Today this monomorphization ergonomics is unchanged; users gain the option of relying on `Ord` conformance.

**Implementation cost.** ~50–80 Rust LOC of new dispatch in the binop analyzer; ~30 Gruel LOC for the cmp.gruel file.

### Net Rust-LOC budget

| Phase | Rust LOC removed | Rust LOC added | Gruel LOC added |
|------|---------|---------|---------|
| 1. Prelude as `std/prelude/` | ~225 (string literal) | ~50 (loader + fallback + flatten) | ~225 (file move) |
| 2. Interfaces → Gruel | ~110 (registry + injection) | ~5 (name lookup) | ~20 |
| 3. Built-in enums → Gruel | ~80 (registry + special ids) | ~30 (variant-by-name in intrinsics) | ~10 |
| 4. Eq/Ord + operator dispatch | — | ~80 (sema dispatch) | ~30 |
| **Total** | **~415** | **~165** | **~285** |

Net: **~250 Rust LOC out, ~285 Gruel LOC in**, plus operator overloading for non-built-in types as a permanent language win.

## Implementation Phases

Each phase ships independently behind the `stdlib_mvp` preview gate, ends with `make test` green, and quotes its own LOC delta in the commit message.

### Phase 1: Prelude as `std/prelude/` directory

- [x] Create `std/prelude/{option,result,char,string}.gruel`, splitting the current `PRELUDE_SOURCE` content by topic.
- [x] Add `PRELUDE_FILES` map in `crates/gruel-compiler/src/prelude_source.rs` mirroring the on-disk files via `include_str!`.
- [x] Implement prelude-scope flattening: prelude files are concatenated into a single virtual source parsed under `FileId::PRELUDE` — preserves the existing top-level-items-go-global behavior unchanged.
- [x] Modify `CompilationUnit::parse()` to load via `assemble_prelude_source` (disk first via `GRUEL_STD_PATH` or upward search; embedded fallback on miss).
- [x] ADR-0073's `is_accessible` carve-out continues to work because all prelude files share `FileId::PRELUDE` (concatenated into one virtual file).
- [x] `Sema`-direct test fixtures continue to work because the embedded fallback is always available.
- [x] Delete the `PRELUDE_SOURCE` constant.
- [x] All 2073 spec tests + 89 UI tests pass.

### Phase 2: Built-in interfaces → Gruel

- [x] ADR-0056 surface syntax verified: `interface Name { fn method(self...) -> RetType; }` with receiver modes `self`, `self: Self`, `self: Ref(Self)`, `self: MutRef(Self)`.
- [x] Created `std/prelude/interfaces.gruel` with `Drop`, `Copy`, `Clone`, `Handle` declarations.
- [x] Removed `inject_builtin_interfaces` from `gruel-air/src/sema/builtins.rs`. Interface declarations now flow through standard `resolve_declarations`; conformance still keys off interned names (`"Copy"`, `"Drop"`, `"Clone"`).
- [x] Deleted `BUILTIN_INTERFACES`, `DROP_INTERFACE`, `COPY_INTERFACE`, `CLONE_INTERFACE`, `HANDLE_INTERFACE`, `BuiltinInterfaceDef`, `BuiltinInterfaceMethod`, `BuiltinIfaceTy`, `BuiltinInterfaceConformance` from `gruel-builtins/src/lib.rs`. Kept a small `BUILTIN_INTERFACE_NAMES` static for breadcrumbs.
- [x] Replaced doc-generator iteration over `BUILTIN_INTERFACES` with static text; `make gen-builtins-docs` and `make check-builtins-docs` clean.
- [x] All 2073 spec tests + 89 UI tests pass.

### Phase 3: Built-in enums → Gruel

- [x] Created `std/prelude/target.gruel` with `Arch`, `Os`, `TypeKind`, `Ownership`. Variant order preserved to match the existing compiler-side mappers (`arch_variant_index`, `os_variant_index`).
- [x] Kept the index-based mappers — they encode an ordering invariant the prelude file matches; intrinsics build `EnumVariant { enum_id, variant_index }` directly. The `EnumId`s come from a new `cache_builtin_enum_ids` step run after `resolve_declarations` (so the prelude's enum decls have been registered).
- [x] Deleted `BUILTIN_ENUMS`, `ARCH_ENUM`, `OS_ENUM`, `TYPEKIND_ENUM`, `OWNERSHIP_ENUM`, `BuiltinEnumDef`, `get_builtin_enum`, `is_reserved_enum_name` from `gruel-builtins/src/lib.rs` (~80 LOC). Kept `BUILTIN_ENUM_NAMES` for breadcrumbs.
- [x] Removed the BUILTIN_ENUMS injection loop from `inject_builtin_types`; kept the `builtin_*_id` cache fields, populated in `cache_builtin_enum_ids`.
- [x] Added `pub prepend_prelude(ast, interner, preview_features)` helper for tests/callers that bypass `CompilationUnit::parse`.
- [x] Updated the doc generator to use static text instead of iterating over the deleted registry.
- [x] All 2073 spec tests + 89 UI tests pass; `test_target_arch_intrinsic_uses_compile_target` updated to call `prepend_prelude`.

### Phase 4: Eq, Ord, and operator desugaring

- [ ] Create `std/prelude/cmp.gruel` with `Ordering`, `Eq`, `Ord`.
- [ ] Add steps 4–5 to the binop analyzer in `crates/gruel-air/src/sema/analysis.rs`. Confirm the dispatch order (primitive → bool → BUILTIN_TYPES registry → Eq/Ord interface → error) keeps existing behavior unchanged.
- [ ] Add spec tests in `crates/gruel-spec/cases/`:
  - User struct with `eq` method: `==` and `!=` work.
  - User struct with `cmp` method: `<`, `<=`, `>`, `>=` work.
  - User struct with neither: clear error message naming `Eq` / `Ord`.
  - Float `==`: still primitive, unchanged.
  - String `==`: still goes through `__gruel_str_eq`, unchanged.
- [ ] `make test`.

### Phase 5: Stabilization

- [ ] Remove the `stdlib_mvp` preview gate (no user-visible feature requires staging).
- [ ] Update ADR status to Implemented.
- [ ] Sweep generated docs (`make gen-intrinsic-docs` etc.) — confirm nothing references `BUILTIN_INTERFACES` or `BUILTIN_ENUMS`.

## Consequences

### Positive

- **Operator overloading lands in the language.** Every user-defined and stdlib-defined struct can now do `==` and `<`. Permanent ergonomic win.
- **Prelude becomes a normal source file tree.** Syntax highlighting, line-level diffs, per-topic files. Adding to it stops requiring escaping.
- **Stdlib gains substance.** `std/prelude/` houses 7 Gruel files of declarations the compiler used to embed in Rust. Future stdlib growth (`std/io`, `std/collections`) follows the same path.
- **Reorderable enum variants.** Once `Arch`/`Os`/`TypeKind`/`Ownership` are name-resolved, contributors can reorder for readability without touching intrinsic codegen.
- **Structural `String`/`Vec` collapse becomes feasible.** The next ADR can assume operator overloading exists and the prelude is a directory; the eventual collapse stops needing special-case operator routing.
- **Lower contributor barrier for declaration changes.** Adding an interface, a target-platform variant, or a prelude function becomes "edit a Gruel file" instead of "edit a Rust registry, a sema injector, and the generated docs."

### Negative

- **Prelude-scope flattening is new behavior.** It's a small mechanism (~30 LOC of file-discovery + per-file parse + scope merge), but it's new — not a relocation. If it has bugs, every program is affected. Mitigated by Phase 1 carrying the same content currently inlined; the loaded behavior should match exactly.
- **Operator desugaring adds an analysis path.** Steps 4–5 of the binop analyzer can fail in new ways (e.g. one operand `Eq`-conforming, the other not). Error messages need to name `Eq` / `Ord` clearly. ~80 Rust LOC of new sema is small but warrants UI tests.
- **`Ordering` is now a load-bearing prelude type.** A user shadowing `Ordering` would break operator desugaring. Same risk profile as `Option` / `Result` today; not a new class of problem.

### Neutral

- **`String` keeps its registry operators.** Step 3 of the binop dispatch wins before step 4 ever runs. `__gruel_str_eq` / `__gruel_str_cmp` keep being called.
- **`String`/`Vec` runtime untouched.** All 31 functions in `gruel-runtime/src/string.rs` and the codegen-inlined Vec methods stay. The follow-up ADR consumes this ADR's deliverables.
- **No spec changes for existing surfaces.** `Option`'s, `Result`'s, `String`'s, `Vec`'s, and the four interfaces' observable behavior is unchanged.
- **No new feature flags surface to users.** `stdlib_mvp` exists only for internal staging.

## Open Questions

1. **`std/prelude/` vs sibling `prelude/`?** This ADR picks `std/prelude/` for resolution-path reuse. Alternative: keep prelude resolution distinct so a user replacing `std/` for a freestanding target doesn't lose the prelude. Resolve during Phase 1; the directory shape is the same either way.
2. **Manifest vs implicit discovery.** Does `_prelude.gruel` list the files in `prelude/` explicitly, or is every `.gruel` file under `prelude/` implicitly part of the prelude? Implicit is simpler; explicit gives a single point of truth. Tilt toward implicit unless ordering issues surface.
3. **Variant-by-name lookup at intrinsic codegen.** Phase 3 hinges on the compiler being able to look up an enum's variant by interned name. Verify this is supported (it is for `Option::Some` etc.); if not, Phase 3 needs a small helper.
4. **Conformance check ordering.** Step 4 of the binop dispatch (Eq fallback) only runs if step 3 (BUILTIN_TYPES registry) misses. Verify that the registry check is cheap (a hashmap lookup) so the new path doesn't slow down existing programs that hit step 1 or step 2.
5. **`PartialEq` / `PartialOrd` for floats.** This ADR ducks the question. If a downstream user wants generic code that includes floats, they'll need something. Probably a follow-up ADR adding `PartialEq`/`PartialOrd` and either re-routing primitive float comparisons through them or keeping the dual track. Not blocking.

## Future Work

- **`String` / `Vec` runtime collapse (next ADR).** Move the 30+ String runtime functions into Gruel as `self.bytes.method()` compositions; eventually drop `STRING_TYPE` from `BUILTIN_TYPES`. Reformulate `Vec(T)` as a comptime-generic struct calling `@alloc`/`@realloc`/`@free`. The win that ADR-0072 anticipated (~490 LOC of `string.rs` retiring) plus ~300 LOC of Vec codegen-method-lowering. Now feasible because operator overloading exists for non-built-in types and the prelude is a directory.
- **Operator desugaring for `String` via Eq/Ord.** Once the next ADR gives `String` `eq` / `cmp` methods, the BUILTIN_TYPES operator entries retire and step 3 of the binop dispatch goes away. ~80 more Rust LOC out.
- **`PartialEq` / `PartialOrd` for floats.** Land separately if needed.
- **Interface bounds on generics.** Currently structural / duck-typed at comptime. Adding `T: Ord` syntax with explicit checking is a separate ADR.
- **`std/io`, `std/process`, `std/env`.** With the stdlib mechanism warm, these are the next obvious surfaces.
- **`std/collections`.** Once `Vec(T)` is Gruel-defined, `HashMap`/`BTreeMap` belong here.

## References

- [ADR-0010: Destructors](0010-destructors.md) — Drop-glue auto-synthesis (relied on for Shift 2)
- [ADR-0020: Built-in Types as Synthetic Structs](0020-builtin-types-as-structs.md) — Synthetic-struct mechanism that this ADR partially retreats from for interfaces and enums
- [ADR-0026: Module System](0026-module-system.md) — Stdlib resolution mechanism reused by Shift 1
- [ADR-0050: Centralized Intrinsics Registry](0050-intrinsics-crate.md) — Pattern model: hardcoded enum + registry, drop entries to relocate behavior
- [ADR-0056: Structural Interfaces](0056-structural-interfaces.md) — Interface surface syntax for Shifts 2 and 4
- [ADR-0059: Drop and Copy Interfaces](0059-drop-and-copy-interfaces.md) — Interface behaviors that stay hardwired by name
- [ADR-0065: Clone and Option](0065-clone-and-option.md) — "Gruel-resident generic enum in prelude" pattern
- [ADR-0070: Result Type](0070-result-type.md) — Same pattern, expanded
- [ADR-0071: char Type](0071-char-type.md) — "Prelude functions for built-in scalar methods" pattern
- [ADR-0072: String as Vec(u8) Newtype](0072-string-vec-u8-relationship.md) — Direct precursor; the runtime collapse it anticipated is the follow-up ADR enabled by this one
- [ADR-0073: Field/Method Visibility](0073-field-method-visibility.md) — Privileged-access carve-out for prelude/stdlib code
- [ADR-0075: Handle Interface](0075-handle-interface.md) — `Handle` declaration moves in Shift 2

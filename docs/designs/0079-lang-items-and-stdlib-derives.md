---
id: 0079
title: Prelude Split, Lang Items, and Prelude-Driven Derives
status: proposal
tags: [prelude, stdlib, interfaces, derives, comptime, lang-items, refactor]
feature-flag: lang_items
created: 2026-05-04
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0079: Prelude Split, Lang Items, and Prelude-Driven Derives

## Status

Proposal

## Summary

Replace name-string-matched compiler hooks (`name == "Drop"`, `name == "Copy"`, …) with an explicit lang-item system: the prelude tags interface declarations with `@lang("drop")`, `@lang("copy")`, etc., and the compiler resolves behavior by tag rather than by name. To make the compiler-stdlib boundary explicit at the *file* layer (not just the API layer), split the prelude out from `std/`: prelude lives at the top-level `prelude/` directory, is implicitly auto-loaded before anything else, and is the *only* place where `@lang(...)` is permitted. `std/` becomes a regular library reachable via `@import("std")`, with no auto-load semantics. Concurrently, move what *can* move out of the compiler into prelude derives — `@derive(Clone)` synthesis becomes a prelude `derive Clone { … }` block using existing comptime intrinsics (`@type_info`, `@field`, `comptime_unroll for`) plus two minimal grammar extensions: allowing `comptime_unroll for` inside struct literals and accepting parenthesized comptime-string expressions as computed field names. The compiler retains exactly the irreducible kernel: implicit-copy decisions, scope-end drop emission, linearity carve-outs, and operator desugaring — all driven by lang-item lookups, all keyed off declarations the prelude owns.

End state: the names `Drop`, `Copy`, `Clone`, `Handle`, `Eq`, `Ord`, `Ordering` are no longer special to the compiler. The prelude could rename `Clone` to `Dup` by changing one line (the `@lang("clone")` tag on the renamed interface) without touching the compiler. Operator overloading goes through whatever interface the prelude has tagged `@lang("op_eq")` / `@lang("op_cmp")`. The `@derive(Clone)` body lives in `prelude/cmp.gruel` and is a short Gruel function any contributor can read and edit. The prelude is small, privileged, and tightly scoped; `std/` becomes a regular library that *uses* prelude items the same way any user program does.

This is the cleanup ADR-0078 left open: ADR-0078 placed prelude inside `std/` and moved the *declarations* of `Drop`/`Copy`/`Clone`/`Handle`/`Eq`/`Ord` into them, but kept the *behavior* in the compiler keyed by interned name and left the prelude/stdlib distinction blurry. ADR-0079 makes both boundaries — file layout and compiler hookup — honest.

## Context

### What's compiler-side today

After ADR-0078, the prelude is a real module *inside* `std/`: `std/prelude/interfaces.gruel`, `cmp.gruel`, `target.gruel` declare interfaces and enums; the compiler reaches them by name. (ADR-0079 moves these to a top-level `prelude/` — see Shift 0.) Specifically:

- `crates/gruel-air/src/sema/conformance.rs:64-74` — `if iface_def.name == "Copy"` … `"Drop"` … `"Clone"` short-circuits.
- `crates/gruel-air/src/sema/analysis.rs:5715`+ — `analyze_comparison` looks for methods named exactly `"eq"` and `"cmp"`, returns of type "the enum named `Ordering`".
- `crates/gruel-air/src/sema/builtins.rs:90-130` — `cache_builtin_enum_ids` interns the names `"Arch"`, `"Os"`, `"TypeKind"`, `"Ownership"`, `"Ordering"` and stores their `EnumId`s for fast intrinsic lookup.
- `crates/gruel-compiler/src/clone_glue.rs` — synthesizes `@derive(Clone)` method bodies in Rust, walks `StructDef.fields`, emits per-field clone IR.

### What `@derive` and comptime can already do

(Read the survey in this PR; the highlights:)

- **`@type_info(T)`** returns a comptime struct exposing `kind: TypeKind`, `fields: [FieldInfo; N]` (with `name`, `ty`), `variants: [VariantInfo; M]`, `name`. ADR-0042.
- **`@field(value, "name")`** is comptime-string-indexed field access. ADR-0042.
- **`@ownership(T)`** returns `Copy`/`Affine`/`Linear`. ADR-0008/0042.
- **`comptime_unroll for x in array { … }`** generates N copies of the body — exactly the per-field/per-variant emission primitive. ADR-0042.
- **`@compile_error("msg")`**, **`@compile_log(...)`**, **`@type_name(T)`**, **`@size_of`**, **`@align_of`** — all comptime-callable.
- **User derives** (ADR-0058): `derive Foo { … }` declares method bodies that get spliced into `@derive(Foo)`-tagged hosts; `Self` resolves to the host type at splicing.
- **Comptime `if`/`else`** in derive bodies works via the standard interpreter — `if @ownership(field.ty) == Ownership::Copy { … } else { … }` evaluates at instantiation time.
- **Conformance check** (ADR-0056) is structural: any type with a method matching an interface's signature conforms. `Drop`/`Copy`/`Clone`/`Handle` don't need their declarations to be special; they just need the compiler to know *which interface name* to ask conformance about for each behavior.

What's *missing* for full prelude-driven derives:

1. **No way to construct a struct from an iterated field list.** Building `Self` with each field cloned currently requires writing the field names literally (`Self { x: …, y: … }`). A derive body can't loop over `@type_info(Self).fields` and emit a struct literal — `comptime_unroll for` works in statement position but not as a struct-literal entry, and there's no syntax for a computed field name. A "list of (name, value) tuples" approach doesn't directly work because each tuple has a different value type per field, so a uniform-typed `Vec` can't hold them.
2. **No "variant-by-name match" intrinsic.** Enum derives need to handle each variant; today this requires hand-written `match` in the derive body, which can't be generated from `@type_info(Self).variants` without something like `comptime_match`.
3. **No way for prelude to "tag" a type as conforming to an interface from inside a derive body.** `@derive(Copy)` today sets `StructDef.is_copy = true` directly in the compiler. A prelude-implemented `derive Copy { … }` has no surface to flip that flag.

The first two are addressable with new intrinsics. The third needs either a "derive-emits-conformance-marker" mechanism or a hardcoded "Copy lang-item is special" carve-out (the compiler can still hardcode *which lang item* drives implicit-copy without hardcoding the name).

### Why now

ADR-0078 just shipped. The asymmetry it left ("declarations in the prelude, behavior compiler-side, keyed by string match — and prelude lives inside stdlib") is uncomfortable but acceptable as an interim. Three pressures argue for cleaning it up before more interfaces accumulate:

1. **Brittleness.** Renaming `Clone` to `Dup` in the prelude today silently breaks the compiler. The string-match path doesn't fail at definition time; it just stops finding the interface. Lang items make the binding explicit.
2. **Compounding.** Every new "compiler-recognized" interface (Iterator, Display, Default, …) adds another string match. The cost of *not* generalizing now grows with every addition.
3. **Privilege bleed.** Prelude lives inside `std/` today, so the privileged-access carve-out (ADR-0073) and any future "this attribute is special" check would have to grep paths or special-case the `prelude/` subdirectory. Promoting prelude to a top-level directory makes it a first-class concept that the compiler and contributors can both point at.

## Decision

Four structural changes.

### Shift 0: Split prelude out from stdlib

Today, after ADR-0078, the prelude lives inside `std/` (`std/_prelude.gruel` and `std/prelude/*.gruel`), alongside ordinary stdlib code (`std/_std.gruel`, `std/math.gruel`). This conflates two different things: the privileged auto-loaded namespace versus a regular library. Move them apart:

```
prelude/                  # NEW top-level directory
  _prelude.gruel          # auto-loaded entry point
  interfaces.gruel
  target.gruel
  cmp.gruel
  option.gruel
  result.gruel
  char.gruel
  string.gruel

std/                      # regular library, @import("std")
  _std.gruel
  math.gruel
  ... (future I/O, collections, etc.)
```

**Loading order:**
1. Compiler loads `prelude/` (every `.gruel` file under it, with `_prelude.gruel` as the root that re-exports submodules). All prelude `pub` items become globally visible without `@import`.
2. `std/` is loaded lazily via `@import("std")` from user code. It can reference prelude items the same way user code does (`Drop`, `Option`, `Result`, etc. are in scope by virtue of the prelude being auto-loaded *first*).
3. User code is parsed, with prelude items resolvable but `std/` items requiring explicit `@import`.

**`@lang(...)` is restricted to the prelude.** Files outside `prelude/` that contain `@lang(...)` directives produce a compile error. This makes the privilege boundary explicit and mirrors the structure of compiler-tagged trait identity in Rust (only `core` / `alloc` / `std` use `#[lang = "..."]`). User code, third-party libraries, and stdlib code use `@derive(...)` and conform to interfaces normally — they just can't claim *new* lang-item bindings. The closed list of recognized lang-item names is still in the compiler (`gruel-builtins/src/lib.rs::LANG_ITEMS`), so even prelude can't invent unrecognized lang-items without a compiler change.

**Why this matters:**
- The privilege carve-out is small and tightly scoped. ADR-0073's privileged-access carve-out for prelude/builtin code now applies to a directory the human can point at.
- Stdlib growth doesn't accidentally accumulate compiler-coupling. Adding `std/io.gruel` or `std/collections/vec.gruel` doesn't get to claim `@lang(...)`; it has to be an ordinary Gruel file using prelude-defined interfaces.
- Renaming or restructuring stdlib has zero impact on the compiler. The compiler only cares about the prelude's lang-item bindings.

### Shift 1: `@lang("name")` attribute and lang-item registry

Add `@lang("string")` as a directive recognized on `interface` and (optionally later) `struct`/`enum`/`fn` declarations. Stdlib tags its compiler-recognized declarations:

```gruel
@lang("drop")
pub interface Drop {
    fn drop(self);
}

@lang("copy")
pub interface Copy {
    fn copy(self: Ref(Self)) -> Self;
}

@lang("clone")
pub interface Clone {
    fn clone(self: Ref(Self)) -> Self;
}

@lang("handle")
pub interface Handle {
    fn handle(self: Ref(Self)) -> Self;
}

@lang("op_eq")
pub interface Eq {
    fn eq(self: Ref(Self), other: Self) -> bool;
}

@lang("op_cmp")
pub interface Ord {
    fn cmp(self: Ref(Self), other: Self) -> Ordering;
}

@lang("ordering")
pub enum Ordering { Less, Equal, Greater }
```

The compiler maintains a `LangItems` struct on `Sema` populated during `resolve_declarations`:

```rust
#[derive(Debug, Default)]
pub(crate) struct LangItems {
    drop: Option<InterfaceId>,
    copy: Option<InterfaceId>,
    clone: Option<InterfaceId>,
    handle: Option<InterfaceId>,
    op_eq: Option<InterfaceId>,
    op_cmp: Option<InterfaceId>,
    ordering: Option<EnumId>,
    // ... more as needed
}
```

The closed enum of recognized lang-item names is in `gruel-builtins` (next to `BUILTIN_INTERFACE_NAMES`); the compiler iterates `@lang("...")` attributes on items, checks the string against the closed set, and records the binding. Unknown lang-item strings produce a compile error. Multiple items claiming the same lang-item produce a compile error.

The compiler then **replaces every name-string match** with a lang-item lookup:

```rust
// Before:
if iface_def.name == "Drop" { … }

// After:
if Some(iface_id) == sema.lang_items.drop { … }
```

Same for `analyze_comparison`'s operator dispatch: `lang_items.op_eq` instead of looking up the symbol `"Eq"`.

### Shift 2: Move synthesizable derive bodies into the prelude

`@derive(Clone)` and `@derive(Copy)` currently run compiler-side. ADR-0058 already supports user-implemented derives that splice methods onto the host. With the existing comptime stack, plus two small grammar extensions, the bodies become Gruel source — living in the prelude alongside the interface declarations they implement.

The extensions both build on machinery the language already has — `comptime_unroll for` and comptime strings — and don't introduce a new intrinsic or closure variant:

1. **`comptime_unroll for … { … }` is permitted as a struct-literal entry.** Today it generates N copies of its body in statement position. In initializer position, each iteration emits one or more field initializers; the surrounding struct literal collects them. Exhaustiveness (every field of the type initialized exactly once after expansion) is the same check the regular struct literal already runs, performed post-expansion.
2. **Computed field name: `(expr): value`.** Inside a struct literal, a parenthesized expression in the field-name slot evaluates at comptime to a string and is used as the field name. Outside `comptime_unroll for`, this is permitted but rarely useful; inside, it's the natural way to spell "the field whose name is `f.name`."

With that, **`derive Clone`** (in `prelude/cmp.gruel`) becomes:

```gruel
derive Clone {
    fn clone(self: Ref(Self)) -> Self {
        Self {
            comptime_unroll for f in @type_info(Self).fields {
                (f.name): @field(self, f.name).clone()
            }
        }
    }
}
```

After expansion against `Self = Foo { a: A, b: B }` this is just:

```gruel
Foo {
    a: @field(self, "a").clone(),
    b: @field(self, "b").clone(),
}
```

— a regular struct literal, type-checked the regular way. The compiler's `clone_glue.rs` (currently ~200 LOC) deletes.

**The "all fields must implement Clone" rule falls out naturally.** Each per-field `.clone()` call is just method dispatch; if the field's type has no Clone implementation, the dispatch fails to resolve and the derive's instantiation produces a normal "no method `clone` for type `T`" error. There's nothing recursive about the synthesis — the derive body is flat, one `.clone()` call per field. The "recursion" is in the runtime call graph, not at synthesis time. (`Copy` types still pass: the structural short-circuit "Copy types auto-conform to Clone" stays, keyed off lang-items rather than the name string.)

This corrects an unforced asymmetry in the current v1: `@derive(Clone)` today only accepts all-`Copy`-field structs because `clone_glue.rs` emits bitwise field reads, not `.clone()` calls. The "recursive clone glue" framing was a misunderstanding — the new derive emits proper `.clone()` calls, and the constraint becomes the obvious one (all fields must impl Clone), with no extra synthesis logic.

For **`derive Copy`**, the body is a no-op (Copy types are bitwise-copied at use sites; the `copy` method never runs at runtime). The validation that "all fields are Copy" moves into the derive body via comptime:

```gruel
derive Copy {
    fn copy(self: Ref(Self)) -> Self {
        comptime {
            for f in @type_info(Self).fields {
                if @ownership(f.ty) != Ownership::Copy {
                    @compile_error("Copy requires all fields to be Copy");
                }
            }
        }
        // Codegen emits a bitwise copy at every use site of a Copy type, so
        // this body never runs. The `copy` method exists only so structural
        // conformance picks up Copy types — same field-by-field shape as
        // Clone but without the recursive `.clone()` calls.
        Self {
            comptime_unroll for f in @type_info(Self).fields {
                (f.name): @field(self, f.name)
            }
        }
    }
}
```

The "tag the type as conforming to Copy so codegen picks bitwise copy" step is the only thing that *can't* be in the prelude — it's a structural fact the type checker queries. We solve it by lang-items: when a type passes structural conformance to whichever interface is tagged `@lang("copy")`, codegen treats it as Copy. No "set the bit from inside the derive" mechanism needed — conformance itself is the bit.

### Shift 3: Compiler retains the irreducible kernel — driven by lang items

Some behaviors *must* live in the compiler. With lang items, they're keyed off interface IDs that the prelude decides:

- **Implicit-copy at use sites** — type checker queries `lang_items.copy()` for "is this type Copy?"
- **Scope-end drop emission** — drop glue inserts `<lang_items.drop>::drop(value)` calls; structural conformance to the drop lang-item drives synthesis when no user `drop` body exists.
- **Linearity carve-out for Handle** — linearity check exempts types conforming to `lang_items.handle()`.
- **Operator desugaring** — `==` / `<` / etc. dispatch through `lang_items.op_eq()` / `lang_items.op_cmp()`. Ordering variant matching uses `lang_items.ordering()`.
- **Default drop synthesis** — when a struct has no user-written `fn drop(self)` but contains droppable fields, the compiler synthesizes a recursive drop. This stays compiler-side because (a) it must run before user code and (b) it's invariant per type. With lang-items the compiler still recognizes the drop interface generically; only the recursion lives in `drop_glue.rs`.

### Net Rust-LOC budget

| Shift | Rust LOC removed | Rust LOC added | Gruel LOC added |
|------|---------|---------|---------|
| 0. Split prelude / `std/` | — | ~30 (path predicate update + `@lang`-only-in-prelude check + `include_dir` split) | — (file moves) |
| 1. Lang-item infrastructure | — | ~80 (parse + registry + lookups) | ~10 (`@lang(...)` attributes in prelude) |
| 2a. Migrate name-matches | ~40 (string compares across sema/codegen) | ~20 (lang-item lookups) | — |
| 2b. Struct-literal grammar extensions | — | ~50 (parser + sema for `comptime_unroll for` in initializer position + `(expr): value` field name) | — |
| 2c. Prelude `derive Clone` | ~200 (clone_glue.rs) | — | ~25 |
| 2d. Prelude `derive Copy` | ~80 (Copy validation logic) | — | ~20 |
| **Total** | **~320** | **~180** | **~55** |

Net **~140 Rust LOC removed**, ~55 Gruel LOC added. The structural value is bigger than the line count: the compiler stops grepping for trait names, and the prelude's privileges are scoped to a directory anyone can point at.

## Implementation Phases

Each phase ships behind the `lang_items` preview gate, ends with `make test` green, and quotes its own LOC delta in the commit message.

### Phase 0: Split prelude out from `std/`

- [x] Move `std/_prelude.gruel` → `prelude/_prelude.gruel`.
- [x] Move `std/prelude/*.gruel` → `prelude/*.gruel`. Update each `@import("prelude/X.gruel")` in `_prelude.gruel` to `@import("X.gruel")` (now sibling, not child).
- [x] Update `crates/gruel-compiler/src/prelude_source.rs`: two separate `include_dir!` trees (`PRELUDE_DIR` rooted at `prelude/`, `STD_DIR` rooted at `std/`). `resolved_prelude()` collects prelude files from `PRELUDE_DIR` and stdlib files from `STD_DIR` separately.
- [x] `CompilationUnit::parse` and `prepend_prelude` already iterate `resolved.prelude_dir` (they don't load `other_std_files` into the implicitly-imported set), so no change needed beyond the resolver split — stdlib only loads via `@import`.
- [x] Update `is_prelude_path` (`crates/gruel-air/src/sema/file_paths.rs`) to check for the top-level `prelude/` directory and exported it for Phase 1 to reuse for the `@lang(...)` privilege check.
- [ ] (Deferred to Phase 1, where `@lang(...)` parsing lands) Parser/sema check: `@lang(...)` in non-prelude files errors. The path predicate is exported and ready.
- [ ] (Deferred to Phase 1) Smoke test for the `@lang(...)`-only-in-prelude error.
- [x] All 2073 spec tests + 89 UI tests pass; the move is purely structural.
- [x] No `@lang(...)` parsing yet (that's Phase 1) — but the path-based gate is exported and in place.

### Phase 1: `@lang("...")` parsing and `LangItems` registry

- [x] Add `lang_items` to `PreviewFeature` in `gruel-util`.
- [x] Recognize `@lang("string")` attribute on `interface`, `enum`, `struct` declarations in the parser (`gruel-parser/src/chumsky_parser.rs`). Extended `DirectiveArg` to accept string literals; threaded `directives` through `EnumDecl` / `InterfaceDecl` AST and the matching RIR `InstData` variants.
- [x] Add a closed list of recognized lang-item names in `gruel-builtins/src/lib.rs`: `LangInterfaceItem` + `LangEnumItem` enums and an `all_lang_item_names()` helper. Unknown names → `InvalidLangItem` compile error at the `@lang(...)` site.
- [x] Add `LangItems` struct to `Sema` (`crates/gruel-air/src/sema/lang_items.rs`) and populate during `resolve_declarations::populate_lang_items` from the parsed directives. Duplicate claims (two interfaces both `@lang("drop")`) → compile error.
- [x] Add `Sema::lang_items()` accessor (lives on the `lang_items` module, available wherever `Sema` is).
- [x] Path-based privilege gate: `@lang(...)` directives outside `prelude/` are rejected with a clear error. Used the host inst span (RIR storage drops the directive's file_id, but the inst span retains it).
- [x] No behavior change yet — registry exists in parallel with name-matching.
- [x] UI tests: `@lang(...)` on a user interface and on a user enum both produce the privilege error.
- [x] Tagged the prelude declarations: `@lang("drop")`/`copy`/`clone`/`handle` on `prelude/interfaces.gruel`, `@lang("op_eq")`/`op_cmp`/`ordering` on `prelude/cmp.gruel`. The prelude registry resolves on every compilation.

### Phase 2a: Migrate compiler name-matches to lang-item lookups

- [x] `crates/gruel-air/src/sema/conformance.rs` — replace `iface_def.name == "Copy"` / `"Drop"` / `"Clone"` short-circuits with `Some(iface_id) == self.lang_items.copy()` / `drop()` / `clone()`.
- [x] `crates/gruel-air/src/sema/analysis.rs::analyze_comparison` — read the dispatch method name out of the `lang_items.op_eq()` / `op_cmp()` interface declaration; fall back to the historical hardcoded `"eq"` / `"cmp"` for compilations that bypass the prelude.
- [x] Prefer `self.lang_items.ordering()` over `self.builtin_ordering_id` for the `Lt`/`Le`/`Gt`/`Ge` desugaring. The cache stays as a fallback for prelude-less builds.
- [x] `has_copy_directive` / `has_clone_directive` / `is_compiler_derive` resolve the directive arg through `self.interfaces` and compare the resulting `InterfaceId` to `lang_items.copy()` / `clone()`. Falls back to the literal name match when the prelude isn't present (preserves the test-fixture path).
- [x] Tagged prelude declarations with `@lang("drop")` etc. (already done in Phase 1).
- [x] All 2073 spec tests + 91 UI tests pass.
- [ ] Smoke test: rename `Clone` → `Dup` in the prelude — deferred (mechanical follow-up; the lang-item indirection is exercised by the existing tests).

### Phase 2b: Struct-literal grammar extensions

Two minimal additions, both consuming primitives the language already has.

- [ ] **`comptime_unroll for … in … { … }` accepted as a struct-literal entry.** Parser change in `crates/gruel-parser/src/chumsky_parser.rs`: extend the struct-initializer rule to alternate between regular `IDENT: expr` entries and a `comptime_unroll for` block. After parse, RIR generation lowers each iteration of the unroll into N field-initializer entries inline.
- [ ] **Computed field name: `(expr): value`** where the parenthesized expression evaluates at comptime to a string. Parser change: in struct-initializer position, accept `(Expr) : Expr` as well as `IDENT : Expr`. Sema validates that the parenthesized expression's type is `comptime_str` and resolves the resulting string against the type's field set at expansion time. Outside `comptime_unroll for` this is permitted (a contributor *could* write `("a"): 42`) but uncommon; inside the unroll it's the natural way to spell `(f.name)`.
- [ ] Exhaustiveness: after expansion, the regular struct-literal exhaustiveness check fires — every field of the type must be initialized exactly once. Missing field, duplicate field, and unknown field all produce existing diagnostics with spans pointing at the unrolled iteration where applicable.
- [ ] Spec tests: hand-written struct literal with `comptime_unroll for` over `@type_info(T).fields` produces the same AIR as the literal-form `T { a: …, b: …, … }`. Tests for missing/duplicate/extra field after expansion (each surfacing a useful diagnostic). Tests for nested types where field types differ (heterogeneous values across iterations type-check independently).

### Phase 2c: Prelude-implemented `derive Clone`

- [ ] Replace `crates/gruel-compiler/src/clone_glue.rs` with a prelude `derive Clone { … }` declaration in `prelude/cmp.gruel`. Body uses the new struct-literal `comptime_unroll for` form + `@field(self, name).clone()`.
- [ ] Delete `clone_glue.rs` and its callers; the synthesis path now flows through normal `@derive(Clone)` machinery (ADR-0058).
- [ ] Verify spec tests for clone (existing in `crates/gruel-spec/cases/types/clone.toml`) all still pass. **Note**: tests that rely on the v1 "all-Copy-fields only" rejection may need to flip — under the new derive, structs with non-Copy-but-Clone-implementing fields *should* be accepted (and their fields cloned via dispatch). Tests that reject "non-Clone field" stay rejecting; tests that reject "non-Copy field" become accepting.
- [ ] Add a spec test covering nested clone: `struct Inner { … }` with `@derive(Clone)`, `struct Outer { i: Inner }` with `@derive(Clone)` — `outer.clone()` produces a deep clone via per-field `.clone()` dispatch.
- [ ] Add a spec test for the failure mode: `@derive(Clone)` on a struct whose field type has no `clone` method must produce a clear "no method `clone` for type `T`" error pointing at the field.
- [ ] Verify the compiler short-circuit "Copy types auto-conform to Clone" still works — keyed off `lang_items.copy()` / `lang_items.clone()`, not name strings.

### Phase 2d: Prelude-implemented `derive Copy`

- [ ] Add `derive Copy { … }` to `prelude/interfaces.gruel`. Body validates all fields are Copy via `@type_info` + `@ownership` + `@compile_error`.
- [ ] Remove the compiler-side `@derive(Copy)` validation logic (it was triggered by name match in `gruel-air/src/sema/declarations.rs`).
- [ ] The "tag the type as Copy" step is replaced by structural conformance: codegen at use sites queries "does T conform to `lang_items.copy()`?" instead of reading `StructDef.is_copy`. Eventually `is_copy` field on `StructDef` is removable (or stays as a cache).
- [ ] All move-semantics spec tests (`crates/gruel-spec/cases/types/move-semantics.toml`) pass.

### Phase 3: Optional — extend derive capabilities for enums

This phase is not strictly required by the lang-items goal, but unlocks future prelude derives (Eq/Ord for enums, Debug, Hash, etc.). It can ship later or split off.

- [ ] Allow `comptime_unroll for v in @type_info(Self).variants { … }` inside a `match` expression to generate variant arms — the same shape as the struct-literal extension in Phase 2b, but for match arms.
- [ ] Each iteration emits one `match` arm. Computed variant name via `(expr) =>` for parity with `(expr): value` in struct literals. Payload binding (when the variant has fields) handled by the same per-field unroll machinery.
- [ ] Validate exhaustiveness at expansion time (every variant covered).
- [ ] Spec test: hand-implement `derive Clone` for an enum using the new match-arm unroll form.
- [ ] Defer this phase to a follow-up if scope creep threatens — the lang-items + struct-derives win is independent.

### Phase 4: Stabilize

- [ ] Remove the `lang_items` preview gate.
- [ ] Update ADR status to Implemented.
- [ ] Update `docs/generated/builtins-reference.md` (regenerate; the interface section now points at `@lang(...)` tags, not BUILTIN_INTERFACE_NAMES).
- [ ] Sweep for residual `name == "Drop"` / `"Copy"` / `"Clone"` strings; convert any remaining to lang-item lookups.

## Consequences

### Positive

- **Compiler-prelude-stdlib boundary becomes honest at the file layer.** Compiler hardcodes mechanisms (drop emission, implicit copy, operator desugaring); prelude hardcodes which interfaces drive each mechanism via `@lang(…)` tags; stdlib is just regular library code with no privilege.
- **Renaming/refactoring becomes safe.** `Clone` → `Dup`? Change one tag binding, done. The compiler doesn't care.
- **`clone_glue.rs` retires.** ~200 LOC of Rust becomes ~25 LOC of Gruel that any contributor can read.
- **`@derive(Clone)` gets the obvious constraint.** All fields must implement Clone; the derive emits `.clone()` per field; method dispatch handles the rest. The current v1 caveat ("all-Copy-fields only") was a compiler-side shortcut, not a real constraint, and goes away.
- **Future derives become possible.** `derive Debug`, `derive Hash`, `derive Default`, etc. — none require compiler changes once the struct-literal `comptime_unroll for` (and later the analogous `match`-arm form for enums) exists.
- **Operator overloading becomes generic.** A future `+` overload via `@lang("op_add")` is a single tag plus a pattern match in `analyze_arith`, not new compiler scaffolding per operator.

### Negative

- **The struct-literal grammar extension is small but load-bearing.** The parser change is straightforward; the sema work is the existing struct-literal exhaustiveness check applied after `comptime_unroll` expansion. Error messages have to point at the iteration site, not the post-expansion virtual line, when something goes wrong (missing field, duplicate field, type mismatch). ~50 Rust LOC for both extensions combined is realistic but not generous.
- **Lang-item validation is a new failure surface.** Missing `@lang("drop")` in the prelude produces a confusing compile error (everywhere drop is used). The error message has to point at the missing tag, not the use site.
- **The closed `LANG_ITEMS` list is still a Rust-side enum.** Stdlib can't introduce a *new* lang-item without a compiler change. This is fine — the meaningful generalization is over the names of *known* mechanisms, not adding new mechanisms.
- **Some test coverage shifts.** Spec tests for "Clone synthesizes the right body" become assertions about the prelude derive emitting the right LLVM IR. UI tests for "@derive(Clone) errors on non-Copy field" need to verify the comptime `@compile_error` message rather than the compiler's bespoke diagnostic.

### Neutral

- **Type constructors (`Vec`, `Ptr`, `Slice`, …) stay compiler-side.** They're language primitives, not interface conformance — orthogonal to this ADR.
- **No spec changes.** User-facing surface is unchanged: `@derive(Clone)` still works, `==` still desugars to `eq()`, `Drop` still runs at scope end.
- **Bootstrap order matters.** Prelude must be parsed and lang-item tags resolved *before* any sema phase that asks "is X the drop interface?". Today's prelude-loaded-first machinery (ADR-0078) ensures this; verify in Phase 1.

## Open Questions

1. **`(expr): value` syntax in struct literals.** Outside a `comptime_unroll for` body, this construct is rarely useful but isn't actively harmful. Decision: accept it everywhere (parser-side simplest), and keep diagnostics generic. Resolve in Phase 2b.
2. **`is_copy` flag on `StructDef`.** Once Copy is structural-conformance-driven, the cached bool is redundant. Removing it touches every codegen site that reads it. Leave it as a cache for Phase 2d; revisit in a cleanup ADR if needed.
3. **Anonymous types.** `@derive(Clone)` on an anonymous struct returned from a comptime function — does the prelude derive body work the same? It should (`Self` resolves to the anon type at splice time, ADR-0058), but verify in Phase 2c.
4. **Coexistence of compiler-side and prelude-side derives during migration.** Phases 2c and 2d each replace one compiler-side derive with a prelude one. The cutover has to be atomic per derive (no half-state where both fire). Plan: each phase removes compiler hardcoding in the same commit that adds the prelude derive.
5. **`@lang(...)` privilege boundary.** Phase 0 adds the path-based check ("only files under `prelude/` can claim `@lang(...)`"). The exact predicate (does it allow nested directories under `prelude/`? what about a `prelude/_macros/` subdirectory?) needs a clear rule. Decision: any file whose path resolves under the top-level `prelude/` directory may use `@lang(...)`. Resolve in Phase 0.
6. **Error UX when lang-item is missing.** If the prelude accidentally drops `@lang("clone")`, every `.clone()` call fails to find the interface. The error needs to be: "lang-item `clone` is not bound — the prelude should declare an interface tagged `@lang(\"clone\")`". Implement in Phase 1's registry.
7. **Match-arm unroll design.** Phase 3 is sketched but not detailed. The right shape (per-variant binding, exhaustiveness, payload destructuring) deserves its own mini-design. Defer until Phase 3 starts.

## Future Work

- **More lang items.** `Iterator` (for `for x in iter` desugaring), `Default` (for `T::default()`), `Display` / `Debug` (for formatting), `Hash` (for hash-map keys). Each becomes a regular interface + `@lang("…")` tag once the infrastructure exists.
- **`+`/`-`/`*`/`/` operator overloading via `@lang("op_add")` etc.** Generalizes the Eq/Ord pattern from ADR-0078 to all binary operators.
- **User-defined attributes / proc macros.** Today only `@derive(...)` and (after this ADR) `@lang(...)` are recognized. A general "user-defined attribute that triggers a comptime function" mechanism would be the next step toward Rust-style proc macros — out of scope here, but the infrastructure (parsed attribute storage, registry of compiler-recognized attributes) lays the groundwork.
- **Retire `clone_glue.rs`, `drop_glue.rs` defaults.** Once derives are user-implementable, the recursive default-drop synthesis could move into stdlib too via `derive AutoDrop { fn drop(self) { comptime_unroll for f in @type_info(Self).fields { @field(self, f.name).drop(); } } }`. Compiler keeps "insert call at scope end"; stdlib keeps the body. Out of scope for this ADR.

## References

- [ADR-0008: Affine Types and the MVS](0008-affine-types-mvs.md) — ownership trichotomy that lang-items respect
- [ADR-0025: Comptime](0025-comptime.md) — comptime evaluator that derive bodies use
- [ADR-0040: Comptime Expansion](0040-comptime-expansion.md) — mutation, enums in comptime
- [ADR-0042: Comptime Metaprogramming](0042-comptime-metaprogramming.md) — `@type_info`, `@field`, `@compile_error`, `comptime_unroll for`
- [ADR-0050: Centralized Intrinsics Registry](0050-intrinsics-crate.md) — where `@lang` registers
- [ADR-0056: Structural Interfaces](0056-structural-interfaces.md) — interface declaration / conformance shape
- [ADR-0058: User-Defined Derives](0058-comptime-derives.md) — `derive Foo { … }` blocks; the substrate for stdlib derive bodies
- [ADR-0059: Drop and Copy Interfaces](0059-drop-and-copy-interfaces.md) — current name-matched compiler hooks
- [ADR-0065: Clone and Option](0065-clone-and-option.md) — Clone's current "all-Copy-fields-only" v1 caveat
- [ADR-0073: Field/Method Visibility](0073-field-method-visibility.md) — privileged-access carve-out for prelude code
- [ADR-0075: Handle Interface](0075-handle-interface.md) — Handle's linear-type carve-out
- [ADR-0078: Stdlib MVP](0078-stdlib-and-prelude-consolidation.md) — direct precursor; declared the asymmetry this ADR fixes
- Rust's lang-item mechanism: <https://rustc-dev-guide.rust-lang.org/lang-items.html> — design reference

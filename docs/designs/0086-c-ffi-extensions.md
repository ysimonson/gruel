---
id: 0086
title: C FFI extensions — C named primitives, enum FFI, static linking
status: proposal
tags: [ffi, types, codegen, linker, grammar]
feature-flag: c_ffi_extras
created: 2026-05-13
accepted:
implemented:
spec-sections: ["10.3", "10.4", "10.5"]
superseded-by:
---

# ADR-0086: C FFI extensions

## Status

Proposal

## Summary

Three extensions to the C FFI surface introduced by [ADR-0085](0085-c-ffi.md):

1. **C named primitive types.** Thirteen new types matching Rust's `core::ffi` set minus `c_char` (whose target-dependent signedness needs its own design): twelve arithmetic primitives (`c_schar`, `c_uchar`, `c_short`, `c_ushort`, `c_int`, `c_uint`, `c_long`, `c_ulong`, `c_longlong`, `c_ulonglong`, `c_float`, `c_double`) plus the incomplete type `c_void` which only composes as `Ptr(c_void)` / `MutPtr(c_void)`. Each arithmetic primitive is *distinct* from its underlying native type but lowers to a target-resolved fixed-width type. `c_int` becomes the canonical discriminant type for `@mark(c) enum`. `MutPtr(c_void)` doubles as the type for `@mark(c) fn` pointer values — `@mark(c) fn` identifiers are castable to `MutPtr(c_void)` via `as`, which unblocks the `@thread_spawn`/`@thread_join` retirement in ADR-0087.
2. **Enum FFI (`@mark(c) enum`).** Lifts the v1 `MarkerNotApplicable` rejection. Field-less enums lower to a bare `c_int`. Data-carrying enums lower to `struct { tag: c_int; union { variants… }; }` — a closed-form C tagged-union layout. Niche optimisation disabled, same as `@mark(c) struct`.
3. **Static linkage.** New sibling keyword `static_link_extern("foo") { … }` parallel to `link_extern`. ELF link line wraps the library with `-Wl,-Bstatic` / `-Wl,-Bdynamic`; Mach-O link line emits `-Wl,-search_paths_first -lfoo` so ld picks the `.a` archive when one is present.

`__gruel_*` runtime cleanup is *not* in this ADR. The successor [ADR-0087](0087-prelude-fns-for-libc-wrappers.md) handles it in full — including the five trivially-shaped libc-shim intrinsics (`@alloc/@free/@realloc/@exit/@memcmp`) that an earlier draft of this ADR had as Phase 5. The cleaner architecture is that codegen-emitted lowerings call prelude fns, and the prelude binds libc via ordinary source-level `link_extern("c")` blocks (which ADR-0085's library-set walker already handles); routing the five libc shims through a special "direct libc codegen + implicit link library set" mechanism would have introduced a parallel link mechanism for no real benefit.

The single preview feature `c_ffi_extras` gates the user-visible surface and retires at stabilisation.

## Context

ADR-0085 shipped C FFI behind `link_extern("…") { … }` and `@mark(c)` (on fns and structs). It explicitly punted three things:

- **Enum FFI.** Field-less and data-carrying `@mark(c) enum` types. The blocker named in ADR-0085 was the absence of a target-dependent `c_int` for the discriminant.
- **Static linkage.** Open Question 5: "syntax TBD (`link_extern(static, "foo") { … }`, `link_extern("foo", kind = "static") { … }`, or similar)." The decision was to ship `-l<name>` only. This ADR picks a third surface — a dedicated `static_link_extern` keyword — keeping the parser-side recognition flat (one keyword per item form, no inner qualifier soup) and leaving `link_extern`'s grammar untouched.
- **`__gruel_*` collapse.** Neutral consequence 1: "cleanup to express them via `link_extern("c") { … }` is mechanical but optional." The follow-up [ADR-0087](0087-prelude-fns-for-libc-wrappers.md) handles this in full — every libc-wrapper-shaped `__gruel_*` symbol migrates to a prelude fn that uses ordinary source-level `link_extern` to bind libc. This ADR doesn't touch the runtime; an earlier draft included a "Phase 5: direct-libc codegen" for the five trivially-shaped shims (`alloc/free/realloc/exit/memcmp`) plus an implicit-link-library-set mechanism, but that route introduced a parallel link mechanism running alongside `link_extern`. The cleaner thing is to let ADR-0087's prelude bind libc via normal `link_extern` for *all* the migrated symbols including those five.

`c_*` types, enum FFI, and static linking are independent in principle but coupled by sequencing: enum FFI needs `c_int`. Bundling them into one ADR avoids three small ADRs that share most of the same machinery.

The capability seam from ADR-0085 (each `link_extern` block as a named lexical witness scope) is preserved unchanged — `static`-mode blocks are still per-library and still capability-addressable in the future ADR.

## Decision

### C named primitive types

Thirteen new built-in primitive types model the C named surface, mirroring Rust's `core::ffi` set minus `c_char` (target-dependent signedness needs its own design — see [Future Work]). Twelve are arithmetic primitives, each target-dependent in *principle* and target-resolved to a fixed width/alignment at compile time. The thirteenth, `c_void`, is an incomplete type usable only through pointers — its rules follow the arithmetic table.

On every blessed target (ADR-0077 list: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin` — all LP64), the arithmetic resolutions are:

| Gruel type | C type | Width | Signed | Underlying lowering |
|---|---|---|---|---|
| `c_schar` | `signed char` | 8 | yes | `i8` |
| `c_uchar` | `unsigned char` | 8 | no | `u8` |
| `c_short` | `short` | 16 | yes | `i16` |
| `c_ushort` | `unsigned short` | 16 | no | `u16` |
| `c_int` | `int` | 32 | yes | `i32` |
| `c_uint` | `unsigned int` | 32 | no | `u32` |
| `c_long` | `long` | 64 (LP64) | yes | `i64` |
| `c_ulong` | `unsigned long` | 64 (LP64) | no | `u64` |
| `c_longlong` | `long long` | 64 | yes | `i64` |
| `c_ulonglong` | `unsigned long long` | 64 | no | `u64` |
| `c_float` | `float` | 32 | — | `f32` |
| `c_double` | `double` | 64 | — | `f64` |

Future targets where the LP64 assumption doesn't hold (e.g. Windows LLP64, where `c_long` is 32-bit) resolve each type accordingly without breaking source.

Each type is *distinct* from its underlying native type. Specifically (taking `c_int` as the worked example; every other type in the table follows identically):

- Assignment, return, and argument passing require the static types to match or to be related by an explicit cast: `let x: c_int = 42 as c_int;` is the canonical form. Integer (and float) literals coerce to the C named type the same way they coerce to any other concrete arithmetic type when the context fixes one (`let x: c_int = 42;` is shorthand for `let x: c_int = 42 as c_int;`).
- `as` casts between any C named type and any other arithmetic type are permitted and follow ADR-0058 truncation/extension rules with respect to the target-resolved width.
- Binary operators require both operands to share the type; `c_int + c_int -> c_int`, `c_int + i32` is a type error. The signed/unsigned C pairs (`c_int`/`c_uint`, `c_long`/`c_ulong`, etc.) are also distinct from each other.
- Each C named type is `Copy`, `Send`, `Sync`, has the same niches as the target-resolved underlying type, and otherwise behaves like its underlying arithmetic type for trait/marker purposes.

In LLVM lowering, each maps to the target-resolved fixed-width type from the table. Codegen never branches on "is this `c_int`?" — by the time codegen runs, the AIR carries the resolved width.

All twelve arithmetic types are permitted in the FFI allowed-types table from ADR-0085 §"Allowed FFI types"; numeric primitives now also include the C named integer and float types alongside `i8`–`i64`, `u8`–`u64`, `isize`, `usize`, `f32`, `f64`. The C named rows are documentary at the source level — same width as a native Gruel type on every current target — but distinguish the FFI signature from a "happens to be the same width" Gruel signature, and remain correct on future targets where the resolution shifts. `Ptr(c_void)` and `MutPtr(c_void)` are also added to the FFI allowed-types table (under the pointer row).

`c_int` specifically is the canonical discriminant type for `@mark(c) enum` (see next section) — C's default `enum` discriminant is signed `int`, and this ADR commits to that shape; `c_uint`-discriminant enums would need a separate convention to disambiguate against the signed default.

#### `c_void`: incomplete type with pointer-only use

`c_void` corresponds to C's `void`. Like Rust's `core::ffi::c_void`, it's an **incomplete type**: it has no values, no size, no alignment, and cannot be instantiated. The rules:

- **Cannot appear as a value type.** No `let x: c_void`, no `c_void` parameters by value, no `c_void` struct fields, no `c_void` return type. Use `()` (unit) for C `void` return values, matching ADR-0085's existing rule.
- **Composes only with pointer types.** `Ptr(c_void)` is `const void *`; `MutPtr(c_void)` is `void *`. Both are allowed in FFI signatures and inside C struct fields and as variables / parameters / return types.
- **No deref, no arithmetic, no index.** `*p` for `p: Ptr(c_void)` is a compile error. Pointer offset (`p + n`) is a compile error. To do pointer arithmetic, cast through a typed pointer (`p as MutPtr(u8)`).
- **Pointer casts permitted.** `MutPtr(T) as MutPtr(c_void)` and `MutPtr(c_void) as MutPtr(T)` are legal `as` casts for any `T`, mirroring C's implicit `void *` conversions. The cast is a no-op at the LLVM level (all pointers share representation on every blessed target). Same for the const variants. The user is responsible for the cast being meaningful — the compiler can't check it.
- **Function-pointer transport.** Any `@mark(c) fn` identifier is castable to `MutPtr(c_void)` via `as` in expression position. This is the type-level commitment that makes function pointers passable to FFI sinks like `pthread_create`'s `void *(*)(void *)` parameter. The `as` cast preserves the function's address; the result is a `MutPtr(c_void)` value that can be passed to C code expecting a function pointer. Casting back (`MutPtr(c_void) as <@mark(c) fn signature>`) is *not* in this ADR — once a `@mark(c) fn` enters `MutPtr(c_void)` you can pass it through but not call it again on the Gruel side. (A typed extern fn pointer type for the round trip is Future Work.)

Function/data pointer ABI parity is assumed on every blessed target (LP64, unified address space). Future Gruel targets where this parity doesn't hold would need a distinct `c_fn_ptr_void` type or analogous; flagged in Future Work.

### Enum FFI via `@mark(c) enum`

`@mark(c)`'s `applicable_to` field widens from `FN_OR_STRUCT` to `FN_STRUCT_OR_ENUM`. The existing `MarkerNotApplicable` rejection on `@mark(c) enum` lifts. Two sub-shapes:

**Field-less enums** (no variant carries any field):

```gruel
@mark(c) enum Color { Red, Green, Blue }
```

- Discriminant type = `c_int`.
- Variant discriminants default to 0, 1, 2, … unless explicitly assigned (`Red = 1, Green = 2, Blue = 4`).
- Explicit discriminants must be `c_int`-representable; out-of-range values are rejected with `FfiEnumDiscriminantOverflow`.
- Layout = bare `c_int`. Size = `sizeof(c_int)`, alignment = `alignof(c_int)`.
- Niches disabled (consistent with `@mark(c) struct`).
- Passing by value across the FFI boundary is permitted.

**Data-carrying enums** (at least one variant carries fields):

```gruel
@mark(c) enum Event {
    Quit,
    KeyPress { code: c_int },
    MouseMove { x: c_int, y: c_int },
}
```

Layout = `{ tag: c_int; payload: union<Variants…> }`, with explicit padding chosen so that `payload` starts at `max(alignof(c_int), max(alignof(variant_i)))`. The payload union is laid out as a C union: each variant's fields occupy the same offset; the union's size = max variant size, alignment = max variant alignment. Total enum size = `payload_offset + payload_size`, rounded up to enum alignment = `max(alignof(c_int), max(alignof(variant_i)))`. Niches disabled.

Mirrors Rust's `#[repr(C, int)]` (the variant noted in the [Rust reference](https://doc.rust-lang.org/reference/type-layout.html#reprc-discriminants)) and is the dominant in-the-wild C tagged-union convention (`SDL_Event`, `LLVMValueRef` sub-shapes). Picking it is a wire-format commitment, same shape as ADR-0085's commitment to struct layout.

Recursive C-FFI-type checks on variant payload fields use the same machinery and same `FfiAggregateHasNonCField` diagnostic as `@mark(c) struct` — variant payloads are exactly the same C-FFI-type discipline as struct fields, just packaged as a union.

Allowed on the FFI boundary: any `@mark(c) enum` whose discriminant is `c_int` (i.e. every `@mark(c) enum` in v2 — there's no other choice yet). Both field-less and data-carrying shapes pass by value via LLVM's default C calling convention; small-enough values go in registers, larger ones via sret/byval as LLVM decides. Rejected: passing a non-`@mark(c)` enum by value (continues to use `FfiTypeNotAllowed`).

`fn __drop` on `@mark(c) enum` is rejected with `FfiAggregateHasDrop`, matching the struct rule.

### Static linkage

A new sibling keyword `static_link_extern` introduces a parallel item form to `link_extern`. Body grammar (items inside the block, body-less-fn rule, implicit `@mark(c)`, `@link_name` override) is identical to `link_extern`; only the link-line treatment differs.

```text
static_link_extern_block := "static_link_extern" "(" STRING ")" "{" item* "}"
```

```gruel
static_link_extern("foo") {
    fn foo_init() -> c_int;
}

link_extern("c") { // dynamic, unchanged from ADR-0085
    fn write(fd: c_int, buf: Ptr(u8), n: usize) -> isize;
}
```

Semantics:

- `link_extern` keeps ADR-0085 behaviour: `-l<name>`, dynamic linkage.
- `static_link_extern` requests static linkage from the system linker. The library name must follow the same rules as the dynamic form (non-empty string literal).
- A library named by both a `link_extern` and a `static_link_extern` block (in the same file or across files) is a compile error (`LinkExternConflictingLinkage`) — a library is either statically or dynamically linked, not both.
- Multiple `static_link_extern("name") { … }` blocks across files merge the same way dynamic blocks do.
- `static_link_extern` blocks do not nest and do not nest inside `link_extern` (or vice versa).

Linker-line construction extends ADR-0085's "deduplicated, lex-sorted library set":

- ELF (`target.is_elf()`): static libs emit `-Wl,-Bstatic -l<name>` and the run of static libs is followed by `-Wl,-Bdynamic` before any dynamic libs (so the `-Bdynamic` bracket re-enables shared linkage for libc and the runtime). The deterministic order is: static libs (lex-sorted), `-Wl,-Bdynamic`, dynamic libs (lex-sorted).
- Mach-O (`target.is_macho()`): static libs emit `-Wl,-search_paths_first -l<name>`. macOS `ld` does not have an ELF-style `-Bstatic`/`-Bdynamic` toggle; `-search_paths_first` causes the linker to scan a search directory for `lib<name>.a` *before* `lib<name>.dylib`, which is the closest fit. If only a `.dylib` is present, the link succeeds dynamically — same outcome as ELF without `-Bstatic`, which is silently dynamic. A diagnostic warning (`StaticLinkMachoFallback`) fires when this happens on macOS, surfaced through the normal warning channel.
- `link_system_with_warnings` gains a parameter carrying the per-library linkage mode; the existing `extra_link_libraries: &[String]` becomes `extra_link_libraries: &[(String, LinkMode)]` where `LinkMode { Dynamic, Static }`.

Static linkage is independent of the library's symbol set — empty static blocks (`static_link_extern("foo") {}`) are permitted and produce the `-Wl,-Bstatic -lfoo -Wl,-Bdynamic` bracket without declarations, useful when symbols are reached indirectly.

### Preview gating

`PreviewFeature::CFfiExtras` (CLI: `c_ffi_extras`). Gate fires on:

- Any C named primitive type token (`c_schar`, `c_uchar`, `c_short`, `c_ushort`, `c_int`, `c_uint`, `c_long`, `c_ulong`, `c_longlong`, `c_ulonglong`, `c_float`, `c_double`) — Phase 1.
- `@mark(c) enum` (Phases 2–3).
- `static_link_extern(…)` keyword (Phase 4).

The preview gate retires in Phase 5.

### Diagnostics

New:

- `FfiEnumDiscriminantOverflow { variant, value }` — explicit discriminant doesn't fit in `c_int`.
- `LinkExternConflictingLinkage { library }` — same library declared `static` in one block and dynamic in another.
- `StaticLinkMachoFallback { library }` — Mach-O static-link request resolved to a `.dylib` because no matching `.a` was found (warning, not error).
- `CFfiExtrasPreviewRequired` — generic preview-gate error.

The ADR-0085 set (`FfiTypeNotAllowed`, `FfiAggregateHasNonCField`, `FfiAggregateHasDrop`, `MarkerNotApplicable`) cover the rest. `MarkCOnEnum` (named in ADR-0085's diagnostics list but implemented as a general `MarkerNotApplicable`) is no longer fired.

## Implementation Phases

### Phase 1: C named primitive types

- [x] Lex/parse the thirteen type names as primitive type names. (Implemented as identifier-based resolution in `resolve_type` / `resolve_type_name` rather than dedicated lexer tokens — the parser already routes unknown identifiers through `TypeExpr::Named` and sema resolves them by name, so dedicated tokens would be redundant.)
- [x] RIR + AIR: thirteen new `TypeKind` variants with tags 21–33 (signed integers 21–25, unsigned integers 26–30, floats 31–32, `c_void` at 33). Per-tag variants composed cleaner with the existing range-based helpers (`is_integer` / `is_signed` / `is_unsigned` / `is_float` / `is_numeric` / `is_copy` / `literal_fits`) than a single `Type::CNamed { kind }` wrapper would have. `c_void` is its own variant; it has no value semantics but joins the layout/Sync/Copy helpers as a sentinel.
- [x] Type resolution for the twelve arithmetic types resolves via `resolve_type`/`resolve_type_name` to the per-tag variant; widths come from the static `Layout::scalar(…)` arms in `layout.rs` keyed by `TypeKind`. The Phase-1 implementation hard-codes LP64 sizes; future LLP64 targets will need the layout arms (and `literal_fits`) parameterised on the target — flagged in Future Work.
- [x] Sema: integer/float literals coerce to each arithmetic C named type via the existing `literal_fits` / `negated_literal_fits` machinery (now extended to recognise tags 21–32). Binary ops use the existing `Type` equality discipline — `c_int + i32` errors because their `Type` values differ. Cast support is via the existing `@cast(x)` intrinsic (ADR-0050) which dispatches on `is_integer()` / `is_float()` and already handles the new tags.
- [x] Sema for `c_void`: `resolve_type` (the public entry point) calls `resolve_type_allow_void` internally and rejects bare `c_void` with an FFI diagnostic; the `Ptr(T)` / `MutPtr(T)` constructor arms call `resolve_type_allow_void` directly for their inner argument, so `c_void` is only ever accepted as a pointer pointee. Inference's `resolve_type_name` is leaky (it doesn't go through sema) so `analyze_alloc` re-validates that a let-binding's resolved type isn't `Type::C_VOID`. Deref/arithmetic rejection on `Ptr(c_void)` falls out of the existing ADR-0028 raw-pointer machinery — `c_void` simply has no LLVM lowering for deref.
- [x] Sema for pointer casts: `MutPtr(T) as MutPtr(c_void)` lives downstream of the `@cast` intrinsic / future raw-pointer cast surface; the pointee-allow-void plumbing here is the foundation that work will use. No standalone cast operator added in this phase.
- [ ] **Deferred** — Sema for fn-pointer cast: `@mark(c) fn` identifiers to `MutPtr(c_void)`. Deferred to ADR-0087's thread-retirement phase since that is the first consumer; ADR-0086 doesn't ship anything that uses the cast itself. Mechanism stays open via the existing `MutPtr(c_void)` plumbing.
- [x] Codegen: each arithmetic type lowers to the LLVM type from the target-resolved width row. `pointer_sized_int_type()` not involved; none of the C named arithmetic types are pointer-sized (`isize`/`usize` cover that). `Ptr(c_void)` / `MutPtr(c_void)` lower to opaque pointer types (LLVM 15+ `ptr`). `c_void`-typed values are unreachable at codegen.
- [x] FFI allowed-types table widens to include all twelve arithmetic types; `Ptr(c_void)` / `MutPtr(c_void)` flow through the existing `is_ptr_const() || is_ptr_mut()` check.
- [x] Spec: new section `docs/spec/src/10-c-ffi/03-c-named-types.md` covering the arithmetic table, distinctness rule, literal coercion, FFI permission, and `c_void`'s incomplete-type rules.
- [x] Spec tests under `cases/types/c_named.toml`: literal coercion (`c_int`/`c_long`/`c_uint`/`c_double` returns), preview-gating (`c_int`/`c_void` both require the preview), `c_void` pointer-only use (`MutPtr(c_void)` allowed, bare `c_void` rejected as a return type and a parameter type), distinctness (`i32` ↔ `c_int`, `c_long` ↔ `c_longlong`), and a libc `abs` roundtrip.
- [x] Preview gate `c_ffi_extras` fires on any C named type token (including `c_void`). `analyze_alloc` re-fires the gate on the resolved var_type to catch the let-annotation path that goes through inference's looser `resolve_type_name`.

### Phase 2: Field-less `@mark(c) enum`

- [ ] Widen `BUILTIN_MARKERS` entry for `c` from `FN_OR_STRUCT` to `FN_STRUCT_OR_ENUM`. Add `ItemKinds::FN_STRUCT_OR_ENUM = 0b111` constant.
- [ ] Sema: when `@mark(c)` applies to an `EnumDef`, set discriminant type to `c_int`. Reject data-carrying variants in this phase (`FfiEnumDataCarryingUnsupported` — a temporary phase-2-only error retired in Phase 3).
- [ ] Sema: validate explicit discriminants fit `c_int` (`FfiEnumDiscriminantOverflow`).
- [ ] AIR + codegen: `@mark(c) enum` lowers to a bare `c_int`. Niches disabled. Layout = `c_int` width + alignment.
- [ ] FFI type validation: `@mark(c) enum` permitted on params/returns of `@mark(c)` fns. Non-`@mark(c)` enums continue to be rejected.
- [ ] Spec: section `10-c-ffi/04-enum-ffi.md` covers field-less case.
- [ ] Spec tests under `cases/items/c-ffi-enum.toml`: roundtrip with libc-style enum (e.g. SDL_SCANCODE-style constants), explicit discriminant, FFI export of a Gruel `@mark(c) enum`-returning fn called from a Gruel `link_extern("c")` caller (self-roundtrip), discriminant-overflow rejection.

### Phase 3: Data-carrying `@mark(c) enum`

- [ ] AIR + codegen: implement the C tagged-union layout (`{ tag: c_int; payload: union<…> }`) — discriminant at offset 0, payload at `max(alignof(c_int), max alignof of variants)`, total size `payload_offset + payload_size` rounded to enum alignment.
- [ ] Sema: lift the `FfiEnumDataCarryingUnsupported` Phase-2 rejection. Validate variant payload fields against the C-FFI-type recursive check (reuse `FfiAggregateHasNonCField`).
- [ ] Codegen: pass/return by value via the C calling convention; LLVM decides register vs sret/byval based on total size.
- [ ] Spec tests: data-carrying roundtrip, nested data-carrying enums-in-structs, non-FFI field in a variant payload (rejection).

### Phase 4: Static linking

- [ ] Lexer: reserve `static_link_extern` as a new keyword alongside `link_extern`.
- [ ] Parser: add the `static_link_extern "(" STRING ")" "{" item* "}"` item form. Item rules inside the block reuse the `link_extern` body parser verbatim.
- [ ] RIR/AIR: `Item::LinkExtern` gains `linkage: Linkage { Dynamic, Static }`. The parser stamps `Static` for `static_link_extern` blocks and `Dynamic` for `link_extern`; the rest of the pipeline treats them uniformly except at link-line construction.
- [ ] Sema: detect conflicting linkage for the same library across blocks (`LinkExternConflictingLinkage`). Reject `static_link_extern` nested inside `link_extern` (or vice versa) with the existing `LinkExternNested`.
- [ ] Compiler: `unit.rs` library-set computation tracks per-library linkage. `CompileOptions.extra_link_libraries` becomes `Vec<(String, Linkage)>`.
- [ ] Linker: `link_system_with_warnings` emits ELF or Mach-O linkage flags per the §"Static linkage" rules. Mach-O fallback to dynamic emits `StaticLinkMachoFallback` warning.
- [ ] Spec: section `10-c-ffi/05-static-linking.md`.
- [ ] Spec tests under `cases/items/c-ffi-static.toml`: round-trip with a static-only test library (provided via `tests/fixtures/libstatic_fixture.a` built by the test harness), conflicting-linkage rejection. ELF and Mach-O paths exercised separately via target-conditional tests.

### Phase 5: Stabilise

- [ ] Remove `PreviewFeature::CFfiExtras`; strip `preview = "c_ffi_extras"` from spec tests.
- [ ] ADR status → `implemented`; record spec sections in frontmatter.
- [ ] ADR-0087 (successor) owns all `__gruel_*` runtime cleanup, including the libc-shim symbols. This ADR neither modifies the runtime nor introduces an implicit-link mechanism.

## Consequences

### Positive

- Enum FFI in both shapes — libc-style field-less enums and `SDL_Event`-style tagged unions both bind without manual struct-of-union wrapping.
- The thirteen C named primitives make FFI signatures self-documenting and target-portable; the same source compiles on hypothetical future targets where `int` isn't 32 bits or `long` isn't 64 bits. `c_void` plus the `@mark(c) fn`-to-`MutPtr(c_void)` cast give a type-safe-ish transport for C function pointers, enough for ADR-0087 to retire `@thread_spawn`/`@thread_join` from the intrinsics registry.
- Static linking closes the largest remaining ADR-0085 Open Question and unblocks workflows that ship pinned-version libraries (audio codecs, sqlite, compiled-in extensions).
- No new linkage mechanism. The library-set walker from ADR-0085 stays the single source of truth for which `-l<name>` (and now `-Wl,-Bstatic -l<name>` / `-Wl,-search_paths_first -l<name>`) flags hit the link line; an earlier draft of this ADR introduced an implicit `link_extern("c") { … }` block to support direct-libc codegen for five intrinsics, but that route was withdrawn in favour of ADR-0087 routing all libc binding through ordinary source `link_extern` in the prelude.
- Capability ADR keeps full freedom over what FFI gating looks like; the static-linkage qualifier is orthogonal to call-site gating.

### Negative

- C tagged-union layout is a wire-format commitment. Once published, the `{ tag: c_int; payload: union<…> }` shape is hard to revise. Mirrors Rust's choice, so the cross-language story is at least familiar.
- Static linking on Mach-O is best-effort: macOS `ld` has no `-Bstatic` toggle, so the static-mode block falls back to dynamic if no `.a` is found in the search path. The fallback warning surfaces it but does not error.
- One new reserved keyword (`static_link_extern`). Visually heavier than a qualifier on `link_extern` but parser-side recognition stays flat — one keyword per item form, no inner argument soup — and `link_extern`'s grammar is untouched. The `static_link_extern` / `link_extern` pair reads naturally and grep-distinguishes the two link modes.
- No runtime cleanup in this ADR — the `__gruel_*` symbols continue to exist in their current shape until ADR-0087 lands. The user's original ask bundled runtime cleanup with the FFI extensions; the cleaner architectural cut is to ship the FFI extensions first and let ADR-0087 do the runtime work as one coherent piece.

### Neutral

- The C named primitive set in this ADR is thirteen types — the full Rust `core::ffi` set minus `c_char`, which is deferred to a follow-up ADR because of target-dependent signedness. `c_void`'s incomplete-type semantics are handled in this ADR (pointer-only use, no values, no deref, no arithmetic).
- The intrinsics registry is untouched by this ADR. ADR-0087 is the one that touches it.

## Open Questions

1. **Mach-O static-link fallback.** Warning is reasonable; some users may want it to be a hard error. Could expose a `--strict-static-linking` CLI flag in a follow-up.
2. **C named type width on hypothetical non-LP64 targets.** When/if Gruel supports a target where C's `int` is 16-bit or `long` is 32-bit (LLP64 Windows), do typed literals still accept arbitrary native-width values? Recommend: literals must fit the target-resolved width — this ADR's literal-coercion rule already enforces it.
3. **`c_long` vs `c_longlong` aliasing.** On LP64 both are 64-bit signed and lower to `i64`. They're nevertheless distinct *types* in this ADR (so a signature with `c_long` doesn't silently equal one with `c_longlong`). Confirmed yes — matches Rust's `core::ffi` and preserves the documentary signal across targets where the two diverge (LLP64).
4. **Spec wire-format publication.** The C tagged-union layout becomes a spec-normative paragraph in chapter 10. Are we ready to commit at the same time we ship Phase 3, or do we want a deprecation window? Recommend commit on Phase 3 stabilisation — the wire shape isn't going to change.

## Future Work

- `c_char` — target-dependent signedness (Linux aarch64 is unsigned; Linux x86_64 and macOS aarch64 are signed). Needs a design pass on whether `c_char` is a distinct type with its signedness resolved per target, a separate signed/unsigned pair, or an alias to one of the existing fixed-signedness types per platform.
- **Typed extern fn pointer types.** This ADR ships `MutPtr(c_void)` as the untyped transport for C function pointers (sufficient for `pthread_create`-style sinks). A proper `extern "C" fn(args) -> ret` type — round-trippable through `as` casts in both directions, signature-checked at the call site — is a follow-up. The void-pointer transport from this ADR is the strictly-weaker version: forward-only, no signature safety, no callable-on-the-Gruel-side after the cast.
- `c_size_t`, `c_ssize_t`, `c_ptrdiff_t`, `c_intptr_t`, `c_uintptr_t` — already covered semantically by Gruel's `usize`/`isize`, but adding the C-named aliases would let FFI signatures match libc headers verbatim.
- Bitfield support in `@mark(c) struct` (Future Work bullet on ADR-0085's list).
- Variadic FFI (`printf`/`scanf`) — still deferred.
- Packed C layout (`@mark(c, packed)`) — still deferred.
- Extern statics inside `link_extern` blocks — still deferred.
- Additional ABIs (`system`, `stdcall`, `vectorcall`, `rust`) — still deferred.
- C header import (`@c_import("foo.h")`) — still deferred.
- Capability-system integration — orthogonal to this ADR.
- **Moving libc-wrapper intrinsics out of the registry into prelude fns.** Successor [ADR-0087](0087-prelude-fns-for-libc-wrappers.md) — every libc-wrapper-shaped `__gruel_*` symbol (heap/exit/memcmp/panic family/dbg/read_line/thread_* targets, plus the algorithmic wrappers `parse_*`, `random_*`, `utf8_*`, `cstr_to_vec`) migrates from intrinsic rows / runtime symbols to regular Gruel prelude fns. Intrinsics contract to "things that genuinely need compiler magic."
- Re-implementing the algorithmic helpers (`parse_*`, `random_*`, `utf8_*`) in Gruel once the stdlib grows the necessary primitives — orthogonal to the ADR-0087 cleanup; could land either before or after.
- `--strict-static-linking` CLI flag to make Mach-O fallback-to-dynamic a hard error.

## References

- [ADR-0005: Preview Features](0005-preview-features.md)
- [ADR-0028: Unchecked Code and Raw Pointers](0028-unsafe-and-raw-pointers.md)
- [ADR-0050: Intrinsics Crate](0050-intrinsics-crate.md) — the intrinsics registry that ADR-0087 contracts (this ADR doesn't touch it).
- [ADR-0054: `usize` indexing](0054-usize-indexing.md)
- [ADR-0061: Generic Pointer Types](0061-generic-pointer-types.md)
- [ADR-0069: Layout Abstraction and Niches](0069-layout-abstraction-and-niches.md) — `@mark(c) enum` extends the same layout-mode mechanism.
- [ADR-0077: Target System](0077-target-system-llvm.md) — `c_*` widths and alignments live next to the per-target queries.
- [ADR-0083: `@mark(...)` directive](0083-mark-directive.md) — `c`'s `applicable_to` widens here.
- [ADR-0084: Send/Sync markers](0084-send-sync-markers.md)
- [ADR-0085: C foreign function interface](0085-c-ffi.md) — direct parent; this ADR closes its enum-FFI and static-linking follow-ups. The runtime-collapse follow-up belongs entirely to ADR-0087.
- [ADR-0087: Prelude fns for libc-wrapper intrinsics](0087-prelude-fns-for-libc-wrappers.md) — successor; owns all `__gruel_*` runtime cleanup. Uses `link_extern` in the prelude rather than introducing an implicit-link mechanism.
- [Rust reference: `#[repr(C, int)]` enum layout](https://doc.rust-lang.org/reference/type-layout.html#reprc-discriminants)
- [Rust `core::ffi`](https://doc.rust-lang.org/core/ffi/index.html) — model for the thirteen C named primitive types. `c_char` deliberately excluded; see Future Work.
- [Apple `ld(1)` man page](https://keith.github.io/xcode-man-pages/ld.1.html) — Mach-O static-linkage flag conventions.

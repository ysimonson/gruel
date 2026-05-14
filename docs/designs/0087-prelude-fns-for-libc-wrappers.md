---
id: 0087
title: Prelude fns for libc-wrapper intrinsics
status: implemented
tags: [intrinsics, prelude, ffi, runtime, refactor]
feature-flag:
created: 2026-05-13
accepted: 2026-05-14
implemented: 2026-05-14
spec-sections: []
superseded-by:
---

# ADR-0087: Prelude fns for libc-wrapper intrinsics

## Status

Implemented — successor to [ADR-0086](0086-c-ffi-extensions.md). All five phases shipped; the registry contracted by retiring the `@read_line`, `@parse_*`, `@random_*`, `@utf8_validate`, `@bytes_eq`, `@alloc`, `@realloc`, `@free` rows in favour of prelude fns in `prelude/runtime_wrappers.gruel` and updating `@dbg`'s lowering to route through prelude `dbg_*` wrappers.

## Summary

The intrinsics registry (ADR-0050) hosts roughly two kinds of rows: ones that need real compiler magic (codegen-emitted lowerings, type dispatch, ABI bridging) and ones that exist because pre-FFI there was no other way to host a libc-wrapper function. This ADR retires the subset of the second kind that is expressible **today** with the FFI surface from ADR-0085 + ADR-0086 plus minor prelude additions. The remaining libc-wrapper rows — the ones that need language features Gruel doesn't have yet — stay as intrinsics until those features land.

Specifically, these intrinsic rows leave: `@read_line`, `@parse_i32/i64/u32/u64`, `@random_u32/u64`, `@utf8_validate`, `@bytes_eq`, `@alloc`, `@free`, `@realloc`, plus `@dbg`'s lowered per-type targets (`__gruel_dbg_*`). Their bodies become prelude fns calling either libc directly (via `link_extern("c")`) or the surviving Rust-runtime helpers (via `link_extern("gruel_runtime")` — ordinary source FFI, handled by ADR-0085's library-set walker, no compiler-side implicit-link mechanism). Their compiler-emitted call sites — Vec(T) codegen for `alloc`/`free`/`realloc`, `@dbg`'s per-argument dispatch arm — switch from emitting intrinsic-mediated calls to emitting direct calls to the prelude fns. As a related cleanup, the same Phase that ships `alloc`/`free`/`realloc` also retires the `__gruel_exit` runtime shim by having main-return codegen call libc `exit` directly (`__gruel_exit` exists only because the main-return path predates ADR-0085's `link_extern`).

Several rows that an earlier draft proposed migrating **stay as intrinsics** because the migration isn't expressible in current Gruel:

- **`@panic` and the compiler-emitted runtime-error panics** (`@panic_no_msg`, `@panic_div_by_zero`, `@panic_intcast_overflow`, `@panic_bounds_check`, `@panic_float_to_int_overflow`, `@panic_vec_dispose`) — moving `@panic(msg)` to a prelude fn would type its parameter as `String`, today's only string type, which is a heap-owning `Vec(u8)` wrapper. The panic path must not require heap-allocated strings (panic is the failure mode for *every* runtime error, including allocator failure); a non-owning string-slice type (`Str` / `&str` / `Slice(u8)`-with-utf8-guarantee) doesn't exist yet. Keeping the family together is cleaner than splitting at the message/no-message line.
- **`@spawn` / `@thread_join`** — `@spawn`'s codegen synthesises a `@mark(c)` marshaling thunk per `(arg-type, return-type, fn)` triple. ADR-0086 ships the `c_void` + `@mark(c) fn`-to-`MutPtr(c_void)` cast that lets the thread *handle* be a plain pointer at the FFI boundary, but synthesising the thunk itself requires extending ADR-0055 Phase 4's comptime-generic monomorphisation to emit `@mark(c)` on the synthesised fn. That extension is genuine new compiler work, not transport.
- **`@cstr_to_vec`** — its runtime ABI uses an out-pointer to a Vec(u8) slot that the callee writes (sret-style for a non-`@mark(c)` aggregate). Declaring the out-pointer as `MutPtr(Vec(u8))` is FFI-legal (pointers to any pointee are allowed), but the prelude body needs an *uninitialised* Vec slot to pass — initialising one and overwriting it leaks the initial Vec's empty-buffer state, and Gruel's `@uninit` machinery is field-by-field, not whole-aggregate. Stays until either the ABI shape changes or whole-aggregate uninit lands.
- **`@dbg`** — keeps its intrinsic row *because of its argument shape*. The user writes `@dbg(42, true, "hello")`; the compiler dispatches per-argument to the appropriate prelude fn (`dbg_i64`, `dbg_bool`, `dbg_str`) with spaces and a newline interleaved. That dispatch is compile-time work that requires inspecting each argument's type — there's no way to write a plain Gruel fn that takes a heterogeneous variadic list and routes each element to a different per-type fn until Gruel grows comptime kind-of-type dispatch.

After this ADR, the intrinsics registry is *smaller* (the libc-wrapper rows it can migrate today are gone), but it is not yet uniform: panic / threads / cstr_to_vec / dbg remain. Each has a tracked prerequisite. Future ADRs clear them as the prerequisites land.

## Context

ADR-0050 set up the intrinsics crate with a closed `IntrinsicId` enum and exhaustive sema/codegen matches. The intent was a single source of truth for compiler-special operations. The unintended consequence: the registry became a dumping ground for anything that wanted to be a libc call, because pre-FFI there was no other way to express the binding.

ADR-0085 introduced `link_extern("…") { … }`. ADR-0086 added `c_int`-and-friends (twelve C named arithmetic primitives plus `c_void`), enum FFI, and static linking. Both are now in place. What's still missing for a full registry contraction:

1. A non-owning string-slice type to type `panic`'s message parameter without dragging heap allocation into the panic path.
2. Comptime monomorphisation extended to emit `@mark(c)` on the synthesised fn (so `@spawn`'s marshaling thunk can be expressed as a prelude method body rather than codegen-emitted).
3. Whole-aggregate `@uninit` or an FFI accommodation for sret returns of non-`@mark(c)` aggregates, so `@cstr_to_vec`'s body can host its own out-param without leaking the initial slot.
4. Comptime kind-dispatch, so `@dbg`'s heterogeneous variadic can become a generic prelude fn.

This ADR migrates everything *not* blocked on (1)–(4) and leaves (1)–(4)'s dependents in the registry. The principle stays the same: **intrinsics carry compiler magic, not transport.** A row earns its place in the registry if it does codegen-emitted lowering of a language feature, compile-time type dispatch, or hits one of the four prerequisites above. A row that exists only because "there's a libc function we want to call" and *has* an expressible Gruel signature today should not be in the registry.

A natural consequence: the user-facing surface changes from `@read_line()` / `@parse_i64(s)` / `@random_u32()` / `@bytes_eq(a,b,n)` / `@alloc(s,a)` to `read_line()` / `parse_i64(&s)` / `random_u32()` / `bytes_eq(a,b,n)` / `alloc(s,a)`. The `@`-prefix becomes a stronger signal for the rows that remain — it now reliably means "compiler magic" for everything except the four prerequisite-blocked rows above.

## Decision

### Rows that leave the registry

Each row below becomes a regular Gruel prelude fn. The intrinsic enum variant is removed; the sema and codegen arms for it are removed; the user-facing surface drops the `@`-prefix. Bodies live in the prelude (`gruel-builtins`, alongside `String` and `Vec`); they bind libc via `link_extern("c")` for direct-libc work and bind the surviving Rust-runtime archive via `link_extern("gruel_runtime")` for the algorithmic helpers that stay in Rust. Both blocks are ordinary source `link_extern` and rely on ADR-0085's library-set walker (no compiler-side implicit-link).

| Intrinsic | New prelude fn | Body shape |
|---|---|---|
| `@read_line()` | `read_line() -> Vec(u8)` | loop `read(0, MutPtr(u8)::from(&mut byte), 1)` until newline or EOF; pushes into Vec(u8) |
| `@parse_i32(s)` etc. | `parse_i32(s: Ref(String)) -> i32` etc. | wraps `__gruel_parse_*` from the runtime archive; calls `s.ptr()` (new method) and `s.len()` |
| `@random_u32()` / `@random_u64()` | `random_u32() -> u32`, `random_u64() -> u64` | wraps `__gruel_random_*` from the runtime archive |
| `@utf8_validate(s)` | `utf8_validate(s: Slice(u8)) -> bool` | wraps `__gruel_utf8_validate(s.ptr(), s.len())`; compares the returned `u8` to `0` |
| `@bytes_eq(a, b, n)` | `bytes_eq(a: Ptr(u8), b: Ptr(u8), n: usize) -> bool` | direct libc `memcmp(a, b, n) == 0 as c_int` |
| `@alloc(size, align)` | `alloc(size: usize, align: usize) -> MutPtr(u8)` | direct libc `malloc(size)`; `align` is currently dropped (libc malloc returns max-aligned) |
| `@free(p, size, align)` | `free(p: MutPtr(u8), size: usize, align: usize)` | direct libc `free(p)`; `size` and `align` dropped |
| `@realloc(p, old, new, align)` | `realloc(p: MutPtr(u8), old: usize, new: usize, align: usize) -> MutPtr(u8)` | direct libc `realloc(p, new)` with null/zero-size handling inline |
| `@dbg`'s per-type lowered targets (`__gruel_dbg_i64`, …) | `dbg_i64`, `dbg_u64`, `dbg_bool`, `dbg_str` + `_noln` variants, `dbg_space`, `dbg_newline` | each is a thin Gruel wrapper around the corresponding `__gruel_dbg_*` runtime fn; `@dbg`'s codegen arm calls these instead of the runtime symbols |

The `@alloc`, `@free`, and `@realloc` intrinsics carry type inference today: `@alloc(size, align)` returns `MutPtr(T)` where `T` is inferred from the binding context; `@realloc(p, …)` preserves `p`'s pointee type. The prelude fns can't reproduce that — they return `MutPtr(u8)` and require callers to bracket with `@ptr_cast` to recover `MutPtr(T)`. The in-tree callers are the Vec(T) bodies in `prelude/vec.gruel`; Phase 4 updates them to wrap each `alloc`/`realloc`/`free` call with explicit casts. User-written `checked` blocks that call `@alloc`/`@realloc`/`@free` directly need the same shape change.

In addition, Phase 4 retires the `__gruel_exit` runtime shim: main-return codegen switches from emitting `call __gruel_exit(code)` to emitting `call exit(code)` against the libc symbol declared in Phase 1's `link_extern("c")` block. This is a runtime-shim removal rather than an intrinsic migration — there is no `IntrinsicId::Exit` to remove; the path was always direct codegen. The libc `exit` declaration in the prelude is typed `-> ()` (FFI doesn't currently allow `-> !`); the codegen attaches LLVM's `noreturn` attribute at the declaration site, the same way it does for `__gruel_exit` today.

Compiler-emitted call sites in codegen rewrite as follows:

- Vec(T) `push`/`reserve`/`clone` bodies (in `prelude/vec.gruel`) update to call the prelude `alloc`/`realloc`/`free` fns wrapped in `@ptr_cast` (see paragraph above). The intrinsic system is no longer in the path.
- `@dbg`'s intrinsic codegen arm calls the new prelude `dbg_*` fns per-argument rather than the runtime symbols directly.
- The main-return path emits `call exit(code)` against the libc symbol rather than `call __gruel_exit(code)` against the runtime shim.

### Rows that stay (with rationale)

**`@panic` family** — `@panic`, `@panic_no_msg`, `@panic_div_by_zero`, `@panic_intcast_overflow`, `@panic_bounds_check`, `@panic_float_to_int_overflow`, `@panic_vec_dispose`. The user-facing `@panic(msg)` takes a string argument. Gruel's only string type today is `String` (a heap-owning `Vec(u8)` wrapper); typing `panic`'s parameter as `String` lets callers pass heap-allocated values into a code path that must remain heap-free, and the language has no way to forbid it. A non-owning string-slice type (`Str` / `&str` / `Slice(u8)`-with-utf8-guarantee) would type the parameter correctly, but it doesn't exist yet. The compiler-emitted no-arg variants (`panic_div_by_zero` etc.) are coupled to the same infrastructure — splitting them out and migrating only those would create an inconsistent half-migration of the panic surface. They all wait for the string-slice ADR.

**`@spawn` / `@thread_join`** — today's `@spawn(fn, arg)` does three things: (1) compile-time validation (arity = 1; arg ≥ Send and not Linear/ref; return ≥ Send), (2) marshaling-thunk synthesis (codegen-emitted `extern "C"` trampoline that boxes `arg`, calls `fn`, boxes the return), and (3) the `pthread_create` call itself. ADR-0086's `c_void` + `@mark(c) fn`-to-`MutPtr(c_void)` cast handles (3) cleanly via a plain prelude fn, and (1) can move into comptime asserts on a generic prelude method body. (2) is the blocker: synthesising a `@mark(c)` thunk per `(A, R, F)` triple requires extending ADR-0055 Phase 4's comptime-generic monomorphisation to allow `@mark(c)` on the synthesised fn. That extension is genuine new compiler work, not transport, and is out of scope for this ADR. Threads stay as intrinsics until that extension lands (or a follow-up ADR designs it).

**`@cstr_to_vec`** — the runtime symbol `__gruel_cstr_to_vec(out: *mut VecU8Result, p: *const u8)` uses an out-pointer to a Vec(u8) slot the callee writes (sret-style for a non-`@mark(c)` aggregate). Declaring the parameter as `MutPtr(Vec(u8))` is FFI-legal, but the prelude body needs an *uninitialised* Vec(u8) slot to pass: initialising one and overwriting it leaks the initial empty-buffer state, and `@uninit` is field-by-field rather than whole-aggregate. Migration waits for either an aggregate-uninit primitive, a redesigned cstr-to-vec ABI that returns a `@mark(c)` value, or some other accommodation.

**`@dbg`** — keeps its intrinsic row *because of its argument shape*. The user writes `@dbg(42, true, "hello")`; the compiler dispatches per-argument to the appropriate prelude fn (`dbg_i64`, `dbg_bool`, `dbg_str`) with spaces and a newline interleaved. That dispatch is compile-time work that requires inspecting each argument's type — there's no way to write a plain Gruel fn that takes a heterogeneous variadic list and routes each element to a different per-type fn until comptime kind-of-type dispatch lands. The body change in this ADR: `@dbg`'s codegen arm lowers each per-argument print to a call to the new prelude `dbg_*` fns rather than a direct call to the matching `__gruel_dbg_*` runtime symbol.

### Prelude additions

The migration needs one small addition to the prelude:

- **`String::ptr(self: Ref(Self)) -> Ptr(u8)`** — a public accessor on the `String` builtin that returns the underlying byte pointer. Body: `checked { self.bytes.ptr() }`. The `bytes` field is non-pub (ADR-0073), so only methods declared inside `prelude/string.gruel` can read it; the new accessor lives there. This unblocks `parse_i32(&s)` etc., which need to extract `(ptr, len)` from a borrowed String to pass to the runtime parse helpers.

`Slice(u8)` already exposes `.ptr()` and `.len()` via the SLICE_METHODS dispatch (ADR-0050's slice-method registry), so `utf8_validate(s: Slice(u8))` works without any additional accessor.

### Prelude organisation and linkage

All prelude fns live in `gruel-builtins`, the crate that today hosts the `String` and `Vec(T)` definitions. The crate gains a new prelude module (`prelude/runtime_wrappers.gruel` or similar — exact name decided in Phase 2) that holds the migrated fns. Naming convention: snake_case, no `__gruel_` prefix, no `c_` prefix — these are user-facing fns and they should read naturally (`read_line`, `parse_i64`, `random_u32`).

The compiler injects these prelude fns into every compilation unit, the same way it injects `String` and `Vec(T)` today. User code can call any of them without an `@import` or `use` statement; they're effectively name-resolution roots.

The prelude binds libc and the Rust-runtime archive via *ordinary source-level* `link_extern` blocks. There is no compiler-introduced "implicit link library set" — ADR-0085's library-set walker already handles every `link_extern` block in every compilation unit, including the ones the prelude contributes. Phase 1 (already implemented) added these blocks to `prelude/runtime.gruel`; the migrated bodies in Phase 2+ reference the symbols declared there.

The `gruel_runtime` library is the static archive `libgruel_runtime.a` that's already linked into every Gruel executable. ADR-0085's library-set walker special-cases the name `"gruel_runtime"` and skips its `-l` emission — the archive is linked by absolute path, and the source-level declaration is purely for sema's binding resolution.

Per ADR-0085 dedup rules, user code is free to declare its own `link_extern("c") { fn malloc(...); }` block; the symbols dedupe against the prelude's declarations. Nothing about the binding mechanism is implicit or compiler-special.

### `@`-syntax removal

Removing `@read_line()`, `@parse_i32(s)`, `@random_u32()`, `@utf8_validate(s)`, `@bytes_eq(a, b, n)`, `@alloc(s, a)`, `@free(p, s, a)`, and `@realloc(p, o, n, a)` syntax in favor of the bare names is a user-visible change. User code in spec tests, runtime examples, and any in-tree Gruel sources is updated as part of Phases 2–4. Because Gruel is pre-1.0 and pre-stable, there's no deprecation window: the old syntax stops parsing the moment the corresponding intrinsic row is removed.

`@panic`, `@spawn`, `@thread_join`, `@cstr_to_vec`, and `@dbg` keep their `@`-prefixed surface for this ADR.

For migration ease, the order is: dbg lowered targets (isolated, no user-visible API change — just rewires `@dbg`'s codegen) → IO/algorithmic (`read_line`, `parse_*`, `random_*`, `utf8_validate`, `bytes_eq` — user-visible API renames but no codegen rewiring) → heap + `__gruel_exit` cleanup (`alloc`, `free`, `realloc` rename plus a switch from `__gruel_exit` to libc `exit` in main-return codegen).

### Why this isn't a preview-feature gated rollout

Preview features (ADR-0005) gate the *addition* of new syntax. This ADR is the *removal* of `@`-prefixed names in favor of bare-fn-name names; the prelude fns are additions but they're idiomatic Gruel fns, not new syntax. There's nothing to gate. The migration is a sequence of user-visible cliffs: each `@`-prefixed name stops parsing the moment its phase lands. Existing in-tree sources are updated in the same commit.

### Diagnostics

No new permanent diagnostics. Phases 2 / 3 / 4 each may temporarily fire a `IntrinsicMigrated { old: "@foo", new: "foo" }` error during the migration window to help any out-of-tree callers find the rename; the diagnostic retires when the phase completes.

## Implementation Phases

### Phase 1: Prelude crate + linkage scaffolding

- [x] New file `prelude/runtime.gruel` carries the link bindings. The existing `_prelude.gruel` manifest gains `@import("runtime.gruel")` as its first entry; `PRELUDE_SUBMODULE_ORDER` in `gruel-compiler/src/prelude_source.rs` lists `runtime.gruel` first so the bindings are visible to every subsequent prelude module. No new injection mechanism — the `String`/`Vec(T)` path already covers prelude source files.
- [x] Prelude `link_extern("c") { … }` block declares `write`, `read`, `exit`, `malloc`, `free`, `realloc`, `memcmp` — the libc bindings every Phase 2–4 migration needs. Ordinary source FFI; the library-set walker emits `-lc`. (No callers yet — Phases 2+ wire them.)
- [x] Prelude `link_extern("gruel_runtime") { … }` block declares the surviving `__gruel_*` algorithmic helpers (`__gruel_parse_{i32,i64,u32,u64}`, `__gruel_random_{u32,u64}`, `__gruel_utf8_validate`). `collect_extern_link_libraries` special-cases `"gruel_runtime"` and skips its `-l` emission — the archive is already linked by absolute path. (`__gruel_dbg_*` declared in Phase 2 when the dbg-lowered-target migration lands.)
- [x] Verified prelude fn name namespace doesn't clash — Phase 1 adds zero prelude fns (just the `link_extern` declarations).

### Phase 2: Migrate `@dbg`'s lowered targets

- [x] Extend `prelude/runtime.gruel`'s `link_extern("gruel_runtime")` block with `__gruel_dbg_i64`, `__gruel_dbg_u64`, `__gruel_dbg_bool`, `__gruel_dbg_str`, `_noln` variants, `__gruel_dbg_space`, `__gruel_dbg_newline`.
- [x] Add prelude fns: `dbg_i64(x: i64)`, `dbg_u64(x: u64)`, `dbg_bool(b: bool)`, `dbg_str(s: Ref(String))`, `_noln` variants, `dbg_space()`, `dbg_newline()`. Each is a one-line wrapper around the corresponding `__gruel_*` symbol. (Bodies live in a new `prelude/runtime_wrappers.gruel` module loaded after `string.gruel` so `Ref(String)` resolves.)
- [x] `@dbg`'s codegen arm rewrites to call the new prelude fns per-argument. Sema-side `analyze_dbg_intrinsic` now also seeds the lazy work queue with the wrappers it dispatches to per arg type — and the post-processing destructor-analysis loop feeds back the `referenced_functions` of each destructor body so a `fn __drop(self) { @dbg(...); }` keeps the wrappers reachable.
- [x] `@dbg` *itself* stays as an intrinsic (compile-time type dispatch — see "Rows that stay").
- [x] Renamed the `link_extern("c")` bindings in `prelude/runtime.gruel` to a `libc_*` prefix (with `@link_name("…")` to bind to the real libc symbols). Bug discovered while running Phase 2's `make test`: the un-prefixed names from Phase 1 (`read`, `write`, `exit`, `malloc`, `free`, `realloc`, `memcmp`) clash with user-written `fn read(…)` etc. and fired `DuplicateTypeDefinition`. The rename is Gruel-side only — the LLVM symbol names still match libc.

### Phase 3: Migrate IO + algorithmic wrappers

- [x] Add `String::ptr(self: Ref(Self)) -> Ptr(u8) { self.bytes.ptr() }` to `prelude/string.gruel`. (Done in Phase 2; the body uses field access rather than the `checked { … }` wrapper sketched in the original ADR — `Vec(T)::ptr` already returns a typed `Ptr(T)` so no `@ptr_cast` is needed.)
- [x] Add prelude fns: `read_line() -> Vec(u8)`, `parse_i32(s: Ref(String)) -> i32` (and i64/u32/u64), `random_u32() -> u32`, `random_u64() -> u64`, `utf8_validate(s: Slice(u8)) -> bool`, `bytes_eq(a: Ptr(u8), b: Ptr(u8), n: usize) -> bool`.
- [x] Sema/codegen: remove the corresponding `IntrinsicId::*` variants and arms (`ReadLine`, `ParseI32`/`I64`/`U32`/`U64`, `RandomU32`/`U64`, `Utf8Validate`, `BytesEq`). Their helper methods (`analyze_*_intrinsic`, `translate_*`, `memcmp_fn`) are deleted along with them.
- [x] Lexer/parser: the `@`-prefixed names stop parsing. (Falls out naturally — the registry no longer has those rows, so the existing `lookup_by_name` returns `None` and the user gets an `UnknownIntrinsic` diagnostic.)
- [x] Update in-tree callers: `prelude/string.gruel`'s `from_utf8` now calls the prelude `utf8_validate(s)` fn (still inside its `checked` block because `@parts_to_slice` is checked-only). No Vec callers needed updating; the `Vec(T)::eq` body uses `==` per-element, not `@bytes_eq`. Spec tests for `@read_line` / `@parse_*` / `@random_*` are deleted along with the spec paragraphs they cited; the prelude fns are library code and don't need normative spec entries (`docs/spec/src/04-expressions/13-intrinsics.md` keeps only the table/sections for the surviving intrinsics).
- [x] Runtime: keep `__gruel_parse_*`, `__gruel_random_*`, `__gruel_utf8_validate` (the prelude fns wrap them); deleted `__gruel_read_line` + its `crates/gruel-runtime/src/io.rs` host + the `File` / `getline` / `stdin` extern declarations + the `stdin` symbol-name shim from `platform.rs`. The new `read_line` prelude body drives libc `read(0, …)` directly.

### Phase 4: Migrate heap + retire `__gruel_exit`

- [x] Add prelude fns: `mem_alloc(size: usize, align: usize) -> MutPtr(u8)`, `mem_free(p: MutPtr(u8), size: usize, align: usize)`, `mem_realloc(p: MutPtr(u8), old: usize, new: usize, align: usize) -> MutPtr(u8)`. Deviation from the ADR's sketched names (`alloc` / `free` / `realloc`): Gruel doesn't mangle user-fn names at the LLVM level, so a prelude `free` emits an LLVM symbol `free` and collides with the libc binding's `@link_name("free")`. The `mem_` prefix breaks the collision; OQ4's longer-qualified name supersedes it once a module system lands. The libc bindings are already in Phase 1's `link_extern("c")` block.
- [x] Update `prelude/vec.gruel` callers: bracket each `mem_alloc`/`mem_realloc`/`mem_free` call with `@ptr_cast` to convert between `MutPtr(T)` and `MutPtr(u8)`.
- [x] Sema/codegen: remove `IntrinsicId::Alloc`, `IntrinsicId::Free`, `IntrinsicId::Realloc`. The corresponding sema helpers (`analyze_alloc_intrinsic`, `analyze_realloc_intrinsic`, `analyze_free_intrinsic`, `require_usize`) and codegen helpers (`translate_alloc`, `translate_realloc`, `translate_free`, `vec_realloc_fn`) are deleted along with them.
- [x] Lexer/parser: `@alloc`/`@free`/`@realloc` stop parsing (registry-driven; falls out of removing the rows).
- [x] Update in-tree user-facing `@alloc`/`@free`/`@realloc` callers — only `crates/gruel-spec/cases/intrinsics/memory.toml` had them. Rewritten to use `mem_alloc` / `mem_free` / `mem_realloc` bracketed by `@ptr_cast`. The `alloc_outside_checked_rejected` case is reframed as `ptr_cast_outside_checked_rejected` since `mem_alloc` itself doesn't require `checked` — the `@ptr_cast` does.
- [x] Codegen: switched main-return from `call __gruel_exit(code)` to `call exit(code)`. `get_or_declare_exit_fn` now declares the LLVM symbol as `exit` and keeps the `noreturn` attribute. The codegen-emitted heap calls inside `@spawn`'s thunk synthesis (the only remaining codegen-side users of `vec_alloc_fn` / `vec_free_fn`) now resolve to libc `malloc` / `free` directly — the `__gruel_alloc(size, align)` shim's `align` parameter is dropped at the call site.
- [x] Runtime: deleted `__gruel_alloc` / `__gruel_free` / `__gruel_realloc` (the FFI entry points; in-crate Rust helpers `heap::alloc` / `heap::free` / `heap::realloc` stay because `__gruel_cstr_to_vec` in `utf8.rs` still uses them) and `__gruel_exit` (codegen no longer emits a call to it; main-return targets libc `exit` directly).

### Phase 5: Stabilise

- [x] Confirmed: `IntrinsicId::ReadLine`, `ParseI32` / `ParseI64` / `ParseU32` / `ParseU64`, `RandomU32` / `RandomU64`, `Utf8Validate`, `BytesEq`, `Alloc`, `Realloc`, `Free` are gone from the enum and `INTRINSICS` table. The `Dbg` row stays but its codegen arm no longer references the `__gruel_dbg_*` runtime symbols directly — those are reached via the prelude `dbg_*` wrappers. The four prerequisite-blocked rows (`Panic` family, `Spawn` / `ThreadJoin`, `CStrToVec`, `Dbg`) remain with the ADR's documented prerequisites.
- [x] Documented the "intrinsics carry compiler magic, not transport" rule in `gruel-intrinsics`'s crate-level docs and added the cross-reference to ADR-0050's Open Questions section.
- [x] ADR status → `implemented` (this checklist is the witness).

## Consequences

### Positive

- The intrinsics registry contracts in the easy direction first. Adding a libc binding that has an expressible Gruel signature is no longer a registry change; it's a prelude fn with a `link_extern` line.
- One linkage mechanism. The prelude binds libc and the runtime archive through ordinary source `link_extern` blocks; the library-set walker from ADR-0085 handles them with no compiler-introduced "implicit link" parallel mechanism.
- The `@`-prefix becomes a sharper signal *for the rows that migrate* — `@read_line` / `@alloc` / `@bytes_eq` and the IO/parse/random family are gone, leaving the prefix on rows that genuinely earn it (compile-time reflection, codegen-emitted lang-items, pointer ops, plus the four prerequisite-blocked rows that this ADR documents).
- The Rust runtime shrinks. After this ADR, `__gruel_read_line`, `__gruel_alloc`, `__gruel_free`, `__gruel_realloc`, `__gruel_exit` disappear; `__gruel_memcmp` was already retired by ADR-0086's direct-libc cleanup. `__gruel_dbg_*` stay but are now reached via prelude wrappers rather than directly. The algorithmic helpers (`parse_*`, `random_*`, `utf8_validate`, `cstr_to_vec`) stay.
- The migration is bounded and reviewable. Each phase touches a coherent slice (dbg targets, IO+algorithmic, heap+exit) with no cross-phase dependencies.

### Negative

- The contraction is partial. `@panic`, `@spawn`/`@thread_join`, `@cstr_to_vec`, and `@dbg` stay as intrinsics. Each has a tracked prerequisite (string-slice type, comptime-`@mark(c)`-fn synthesis, aggregate uninit, comptime kind-dispatch); the "intrinsics carry compiler magic, not transport" rule isn't fully realised until those land.
- One user-visible cliff per phase: the migrated `@`-prefixed names stop parsing the moment their phase lands. Pre-1.0 Gruel has no compatibility commitment, but in-tree callers (spec tests, examples) need to update in lockstep.
- The prelude grows a `link_extern("gruel_runtime")` block (already added in Phase 1). The runtime archive was already implicitly linked today; making the binding source-visible is honest, but means the prelude is now load-bearing for the runtime contract.
- `String::ptr()` joins `String`'s public API. Pre-1.0 the surface is small enough that this isn't a stability concern, but it does expose the underlying byte pointer; callers that hold the raw `Ptr(u8)` past the String's lifetime (or across a `push`/`clear`) will see use-after-free or stale-pointer bugs. The accessor returns `Ptr(u8)` (const, not mut) to limit some of that, and stays inside `checked`-discipline call sites by convention.
- Prelude fn name pollution. `read_line`, `alloc`, `free`, `realloc`, `exit`, `bytes_eq`, `parse_i32` etc. are now globally bound. User code that wants to define a fn with one of those names has to pick a different name or wait for the module system to land.

### Neutral

- The Rust-runtime archive stops being the source of truth for "what symbols the compiler relies on" for the migrated rows — that truth moves to the prelude. The unmigrated rows (panic, threads, cstr_to_vec) keep their existing arrangement.
- No runtime-archive size change for the surviving algorithmic helpers (parse, random, utf8_validate, cstr_to_vec); only the symbols whose bodies were libc shims (`__gruel_alloc`, `__gruel_free`, `__gruel_realloc`, `__gruel_exit`, `__gruel_read_line`) disappear.
- `@dbg`'s observable behavior doesn't change — only the layer that owns its per-argument body shifts from the Rust runtime to the prelude (which still calls the Rust runtime's formatting helpers). The migration is preparation, not user-facing change.

## Open Questions

1. **`parse_*` parameter shape.** This ADR commits to `parse_i32(s: Ref(String))` — explicit borrow. Today's intrinsic `@parse_i32(s)` borrows implicitly via `analyze_inst_for_projection`. Should the prelude fn instead take `String` by value (consuming) to match "prelude fns are ordinary fns" idiom, or by `Ref(String)` to preserve semantics? Recommend `Ref(String)` — preserves the non-consuming intent and the explicit `&` is a minor ergonomic cost.
2. **`align` parameter on `alloc`/`free`/`realloc`.** Libc `malloc`/`free`/`realloc` don't take an alignment. Vec(T) codegen passes alignments today. Should the prelude fns ignore alignment (status quo behavior, fine for current uses) or eventually route through `posix_memalign` / `aligned_alloc` based on the alignment value? Recommend ignore-for-now with a comment; revisit if/when alignment > 16 use cases appear.
3. **`@dbg_str`'s parameter type.** The runtime symbol `__gruel_dbg_str` takes `(ptr, len)`. The prelude `dbg_str` fn should take `Ref(String)` and extract `(s.ptr(), s.len())`, or `Slice(u8)` and extract similarly, or both? Recommend `Ref(String)` for the primary surface (matches what `@dbg` callers see today); a `dbg_bytes(s: Slice(u8))` follow-up is a separate question.
4. **Prelude fn name conflicts in user code.** Pre-module-system, names like `alloc` and `exit` are global. Should the prelude fns be addressable under a longer-qualified name (e.g. `prelude::alloc`)? Recommend yes once the module system arrives; pre-module-system the shorter names are the only names.
5. **Migration order for in-tree callers.** Each phase touches in-tree spec tests / examples; should each phase be one large PR that updates everything atomically, or staged with a temporary `@<name>` alias that calls the prelude fn? Recommend atomic — pre-1.0 lets us avoid the alias complexity.

## Future Work

- **Non-owning string slice type (`Str` / `&str` / `Slice(u8)`-with-utf8-guarantee).** The blocker for `@panic` migration. When this lands, `@panic` follows the same migration pattern (prelude fn `panic(msg: Str) -> !`, runtime-error variants either join it or stay as compiler-emitted no-arg calls to canned-message fns).
- **Comptime monomorphisation of `@mark(c)` synthesised fns.** The blocker for `@spawn`/`@thread_join` migration. Extension to ADR-0055 Phase 4; once it lands, threads follow the prelude-fn pattern with a `JoinHandle(R).spawn(comptime A, comptime F, f: F, arg: A)` comptime-generic method body that calls a plain `thread_spawn(thunk, arg_box, ret_size)` prelude fn over libc `pthread_create`.
- **Whole-aggregate `@uninit` or sret-of-non-`@mark(c)` FFI.** The blocker for `@cstr_to_vec` migration. Either lets a prelude `cstr_to_vec(p: Ptr(u8)) -> Vec(u8)` body host an uninitialised Vec slot or pass the slot through the FFI cleanly.
- **Comptime kind-dispatch for `@dbg`.** Allows `@dbg` itself to leave the registry and become a generic `dbg<T>(x: T)` prelude fn — the last libc-wrapper-shaped row clears at that point.
- **Re-implementing the surviving algorithmic helpers** (`parse_*`, `random_*`, `utf8_*`, `cstr_to_vec`) in Gruel once the stdlib grows the necessary primitives — orthogonal to this ADR's intrinsic-registry cleanup but related: it eventually deletes the prelude's `link_extern("gruel_runtime")` block entirely (the runtime archive itself goes away once nothing in the prelude wraps it).
- **Module-system migration** when ADR-TBD lands: the prelude moves from compiler-injected names to `use std::prelude::*`-style auto-import.

## References

- [ADR-0005: Preview Features](0005-preview-features.md) — explicitly *not* used here; this is removal of syntax, not addition.
- [ADR-0020: Built-in Types as Structs](0020-builtin-types-as-structs.md) — the injection mechanism that prelude fns reuse.
- [ADR-0050: Intrinsics Crate](0050-intrinsics-crate.md) — the registry whose libc-wrapper rows leave here.
- [ADR-0055: Comptime type-arg inference](0055-comptime-type-arg-inference.md) — the Phase 4 monomorphisation pass that a future `@spawn` migration would extend.
- [ADR-0072: String / Vec(u8) relationship](0072-string-vec-u8-relationship.md) — owns `String`'s public API surface, which this ADR extends with `String::ptr()`.
- [ADR-0073: Field / method visibility](0073-field-method-visibility.md) — the `bytes`-field privacy rule that makes `String::ptr()` necessary (instead of letting prelude fns destructure directly).
- [ADR-0084: Send/Sync Markers](0084-send-sync-markers.md) — the `Send`/`Sync` checks that a future `@spawn` migration would lift into comptime asserts on `JoinHandle.spawn`.
- [ADR-0085: C foreign function interface](0085-c-ffi.md) — `link_extern` is what makes this migration possible.
- [ADR-0086: C FFI extensions](0086-c-ffi-extensions.md) — direct parent. The `c_int`-and-friends + `c_void` work it ships is what lets `bytes_eq` / `exit` / `alloc`-family prelude bodies type-check.

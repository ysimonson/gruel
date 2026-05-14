---
id: 0087
title: Prelude fns for libc-wrapper intrinsics
status: proposal
tags: [intrinsics, prelude, ffi, runtime, refactor]
feature-flag:
created: 2026-05-13
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0087: Prelude fns for libc-wrapper intrinsics

## Status

Proposal — successor to [ADR-0086](0086-c-ffi-extensions.md).

## Summary

The intrinsics registry (ADR-0050) currently hosts roughly two kinds of rows: ones that need real compiler magic (codegen-emitted lowerings, type dispatch, ABI bridging) and ones that exist because pre-FFI there was no other way to host a libc-wrapper function. This ADR retires the second kind. The libc-wrapper rows leave the registry and become regular Gruel prelude functions that user code can call directly without the `@`-prefix; the rows that genuinely need compiler magic stay.

Specifically, these intrinsic rows leave: `@panic` (and the family of compiler-emitted runtime-error panics), `@read_line`, `@parse_i32/i64/u32/u64`, `@random_u32/u64`, `@cstr_to_vec`, `@utf8_validate`, `@memcmp`, `@alloc`, `@free`, `@realloc`, plus `@thread_spawn` (currently `IntrinsicId::ThreadSpawn`, user surface `@spawn(fn, arg)`) and `@thread_join` (currently `IntrinsicId::ThreadJoin`, called from `JoinHandle::join`). Their bodies become prelude fns calling either libc directly (via `link_extern("c")`) or the surviving Rust-runtime algorithmic helpers (via `link_extern("gruel_runtime")` — ordinary source FFI, handled by ADR-0085's library-set walker, no compiler-side implicit-link mechanism). The thread retirements lean on ADR-0086's `c_void` + `@mark(c) fn`-to-`MutPtr(c_void)` cast for ABI-bridging that previously required compiler magic. Their compiler-emitted call sites — main-return for `exit`, overflow checks for the panic family, Vec(T) codegen for `alloc/free/realloc`, drop glue for the various panic variants — switch from emitting intrinsic-mediated calls to emitting direct calls to the prelude fns.

Of the libc-wrapper-shaped rows this ADR considers, only `@dbg` keeps its intrinsic status — type-dispatch on argument list is compile-time work that can't be expressed as a plain Gruel fn until comptime kind-dispatch arrives. Its body moves to per-type prelude fns; the intrinsic carries only the dispatch logic, not the body. Outside the libc-wrapper category, the registry's other ~40 rows (compile-time reflection, pointer ops, slice ops, vec-literal codegen, lang-item helpers, etc.) are all real compiler magic and stay untouched — see §"Rows that stay (with rationale)".

After this ADR, the intrinsics registry is small and uniform: every remaining row does real compiler work. Adding a libc-shaped operation is no longer an intrinsics-registry change — it's a prelude fn that takes a `link_extern` line.

## Context

ADR-0050 set up the intrinsics crate with a closed `IntrinsicId` enum and exhaustive sema/codegen matches. The intent was a single source of truth for compiler-special operations. The unintended consequence: the registry became a dumping ground for anything that wanted to be a libc call, because pre-FFI there was no other way to express the binding.

ADR-0085 introduced `link_extern("…") { … }`. ADR-0086 added `c_int`-and-friends (twelve C named arithmetic primitives plus `c_void`), enum FFI, and static linking. The `c_void` work in ADR-0086 includes one ABI commitment that ripples directly into this ADR: `@mark(c) fn` identifiers are castable to `MutPtr(c_void)` via `as`. That cast is the missing piece that lets `@thread_spawn`/`@thread_join` leave the intrinsics registry without waiting on a full typed-extern-fn-pointer design — the thunk a user passes to a `thread_spawn(thunk: MutPtr(c_void), …)` prelude fn is just a `@mark(c) fn` they wrote and cast.

An earlier draft of ADR-0086 proposed a three-way split where some intrinsic bodies stayed in Rust, some lowered direct to libc, and some moved to Gruel prelude fns. That draft was withdrawn because it introduced inconsistency without resolving the root issue: as long as libc-wrapper functions live in the intrinsics registry at all, the registry will keep growing whenever a new libc binding is wanted. The root fix is to take those rows out of the registry entirely, which is what this ADR does.

The principle this ADR commits to is: **intrinsics carry compiler magic, not transport.** A row earns its place in the registry if it does codegen-emitted lowering of a language feature (drop glue, Vec.push) or compile-time type dispatch (`@dbg`'s overload-by-type). A row that exists only because "there's a libc function we want to call" — or because "there's a C ABI signature we need to manually marshal arguments through" — is not magic; it's a function, and the FFI surface from ADR-0085 + the `c_void` transport from ADR-0086 lets us write it as one.

A natural consequence: the user-facing surface changes from `@panic("msg")` / `@read_line()` / `@parse_i64(s)` to `panic("msg")` / `read_line()` / `parse_i64(s)`. The `@`-prefix becomes a stronger signal — it now reliably means "the compiler does something special here", not "this happens to be a function the runtime owns." That signal is what the prefix was for in the first place.

## Decision

### Rows that leave the registry

Each row below becomes a regular Gruel prelude fn. The intrinsic enum variant is removed; the sema and codegen arms for it are removed; the user-facing surface drops the `@`-prefix. Bodies live in the prelude (`gruel-builtins`, alongside `String` and `Vec`); they bind libc via `link_extern("c")` for direct-libc work and bind the surviving Rust-runtime archive via `link_extern("gruel_runtime")` for the algorithmic helpers that stay in Rust. Both blocks are ordinary source `link_extern` and rely on ADR-0085's library-set walker (no compiler-side implicit-link).

| Intrinsic | New prelude fn | Body shape |
|---|---|---|
| `@panic(msg)` | `panic(msg: Str) -> !` | `write(2, msg.ptr, msg.len); exit(101)` |
| `@panic_no_msg()` (compiler-emitted) | `panic_no_msg() -> !` | `exit(101)` |
| `@panic_div_by_zero()` (compiler-emitted) | `panic_div_by_zero() -> !` | canned msg + `exit(101)` |
| `@panic_intcast_overflow()` (compiler-emitted) | `panic_intcast_overflow() -> !` | canned msg + `exit(101)` |
| `@panic_bounds_check()` (compiler-emitted) | `panic_bounds_check() -> !` | canned msg + `exit(101)` |
| `@panic_float_to_int_overflow()` (compiler-emitted) | `panic_float_to_int_overflow() -> !` | canned msg + `exit(101)` |
| `@panic_vec_dispose()` (compiler-emitted) | `panic_vec_dispose() -> !` | canned msg + `exit(101)` |
| `@read_line()` | `read_line() -> Vec(u8)` | loop `read(0, buf, 1)` until newline; grows via the prelude's `alloc`/`realloc` |
| `@parse_i32(s)` etc. | `parse_i32(s: Str) -> i32` etc. | wraps `__gruel_parse_*` from the runtime archive |
| `@random_u32()` / `@random_u64()` | `random_u32() -> u32`, `random_u64() -> u64` | wraps `__gruel_random_*` from the runtime archive |
| `@cstr_to_vec(p)` | `cstr_to_vec(p: Ptr(u8)) -> Vec(u8)` | wraps `__gruel_cstr_to_vec` |
| `@utf8_validate(p, n)` | `utf8_validate(p: Ptr(u8), n: usize) -> bool` | wraps `__gruel_utf8_validate` |
| `@memcmp(a, b, n)` | `memcmp(a: Ptr(u8), b: Ptr(u8), n: usize) -> c_int` | direct libc `memcmp` via `link_extern("c")` |
| `@alloc(size, align)` | `alloc(size: usize, align: usize) -> MutPtr(u8)` | direct libc `malloc(size)`; align dropped |
| `@free(p, size, align)` | `free(p: MutPtr(u8), size: usize, align: usize)` | direct libc `free(p)`; size and align dropped |
| `@realloc(p, old, new, align)` | `realloc(p: MutPtr(u8), old: usize, new: usize, align: usize) -> MutPtr(u8)` | direct libc `realloc(p, new)` with null/zero-size handling inline |
| `@spawn(fn, arg)` (`IntrinsicId::ThreadSpawn`) | `thread_spawn(thunk: MutPtr(c_void), arg: MutPtr(c_void), ret_size: usize) -> MutPtr(c_void)` + the higher-level `JoinHandle::spawn` constructor (see §"Thread API shape" below) | wraps `pthread_create` directly via `link_extern` |
| `@thread_join(h)` (`IntrinsicId::ThreadJoin`) | `thread_join(handle: MutPtr(c_void), ret_out: MutPtr(c_void), ret_size: usize)` | wraps `pthread_join` directly via `link_extern`, copies the return value via `memcpy` (which is `memcmp`'s libc neighbour — declared in the same `link_extern("c")` block) |

Compiler-emitted call sites in codegen rewrite as direct prelude-fn calls. For example: the main-return path emits a call to `exit(code)` (prelude fn) rather than `@exit(code)` (intrinsic); Vec(T).push emits a call to `alloc`/`realloc` (prelude fns); the bounds-check pass emits a call to `panic_bounds_check()` (prelude fn). The intrinsic system is no longer in the path for these lowerings.

### Rows that stay (with rationale)

**`@dbg(args...)`** — keeps its intrinsic row *because of its argument shape*. The user writes `@dbg(42, true, "hello")`; the compiler dispatches per-argument to the appropriate prelude fn (`dbg_i64`, `dbg_bool`, `dbg_str`) with spaces and a newline interleaved. That dispatch is compile-time work that requires inspecting each argument's type — there's no way to write a plain Gruel fn that takes a heterogeneous variadic list and routes each element to a different per-type fn until Gruel grows comptime kind-of-type dispatch.

The body change: `@dbg`'s codegen arm now lowers each per-argument print to a call to the new prelude fn (`dbg_i64`, `dbg_u64`, `dbg_bool`, `dbg_str`, plus `_noln` variants and `dbg_space`/`dbg_newline`) rather than a direct call to the matching `__gruel_dbg_*` runtime symbol. The Rust runtime's `dbg_*` symbols disappear. Future ADR migrates `@dbg` itself once kind-dispatch lands.

### Thread API shape

Today's `@spawn(fn, arg)` does three things at once: (1) compile-time validation (arity = 1, arg ≥ Send and not Linear/ref, return ≥ Send), (2) marshaling-thunk synthesis (codegen-emitted `extern "C"` trampoline that boxes `arg`, calls `fn`, boxes the return), and (3) the `pthread_create` call itself. ADR-0086's `c_void` + `@mark(c) fn` cast handle (3) cleanly via a plain prelude fn; (1) and (2) move into a comptime-generic prelude method that uses the same machinery Gruel already has for comptime-typed parameter inference (ADR-0055 Phase 4, `comptime F: type, f: F`):

```gruel
fn JoinHandle(comptime R: type) -> type {
    struct {
        handle: MutPtr(c_void),
        @mark(linear),

        fn spawn(comptime A: type, comptime F: type, f: F, arg: A) -> JoinHandle(R) {
            // 1. Comptime validation: bound checks on A's and R's thread-safety markers,
            //    arity check on F (synthesised in sema, not user-written).
            // 2. Comptime-generated extern-C thunk:
            //    @mark(c) fn thunk(arg_box: MutPtr(c_void)) -> MutPtr(c_void) { ... }
            //    The compiler synthesises one thunk per (A, R, F) triple via the same
            //    monomorphisation pass that handles other comptime-generic fns.
            // 3. Box `arg`, call thread_spawn(thunk, arg_box, @size_of(R)), wrap handle.
        }

        fn join(self) -> R {
            let ret_box: MutPtr(R) = alloc(@size_of(R)) as MutPtr(R);
            thread_join(self.handle, ret_box as MutPtr(c_void), @size_of(R));
            let r: R = *ret_box;
            free(ret_box as MutPtr(u8), @size_of(R), @align_of(R));
            r
        }
    }
}
```

User-facing surface migrates from `@spawn(worker, Job { id: 1 })` → `JoinHandle(WorkerResult).spawn(worker, Job { id: 1 })`. A small `spawn(f, arg)` free fn in the prelude with `comptime R = @return_type(F)` inference can recover the old shape if we want it — but the more explicit form is the one this ADR commits to. The change is a single-line rewrite in spec tests and examples.

The thunk synthesis inside `JoinHandle::spawn` is the one piece of new compiler work that isn't covered by ADR-0086 alone: the compiler must materialise a `@mark(c) fn` body whose `f` reference is comptime-monomorphised. The mechanism is the same one comptime-generic methods already use — see Phase 4 of ADR-0055 — extended to allow `@mark(c)` on the synthesised fn so the resulting thunk has the right ABI. Open Question 2 below tracks whether that extension lands cleanly in this ADR's scope or needs its own micro-ADR.

### Prelude organisation and linkage

All prelude fns live in `gruel-builtins`, the crate that today hosts the `String` and `Vec(T)` definitions. The crate gains a new module (or several — exact layout TBD in Phase 1) that holds the migrated fns. Naming convention: snake_case, no `__gruel_` prefix, no `c_` prefix — these are user-facing fns and they should read naturally (`panic`, `read_line`, `parse_i64`, `random_u32`).

The compiler injects these prelude fns into every compilation unit, the same way it injects `String` and `Vec(T)` today. User code can call any of them without an `@import` or `use` statement; they're effectively name-resolution roots.

The prelude binds libc and the Rust-runtime archive via *ordinary source-level* `link_extern` blocks. There is no compiler-introduced "implicit link library set" — ADR-0085's library-set walker already handles every `link_extern` block in every compilation unit, including the ones the prelude contributes. The prelude source contains, in essence:

```gruel
link_extern("c") {
    fn write(fd: c_int, buf: Ptr(u8), n: usize) -> isize;
    fn read(fd: c_int, buf: MutPtr(u8), n: usize) -> isize;
    fn exit(code: c_int) -> ();
    fn malloc(size: usize) -> MutPtr(u8);
    fn free(p: MutPtr(u8)) -> ();
    fn realloc(p: MutPtr(u8), n: usize) -> MutPtr(u8);
    fn memcmp(a: Ptr(u8), b: Ptr(u8), n: usize) -> c_int;
}

link_extern("gruel_runtime") {
    fn __gruel_parse_i32(buf: Ptr(u8), len: usize) -> i32;
    fn __gruel_parse_i64(buf: Ptr(u8), len: usize) -> i64;
    fn __gruel_parse_u32(buf: Ptr(u8), len: usize) -> u32;
    fn __gruel_parse_u64(buf: Ptr(u8), len: usize) -> u64;
    fn __gruel_random_u32() -> u32;
    fn __gruel_random_u64() -> u64;
    fn __gruel_cstr_to_vec(out: MutPtr(u8), p: Ptr(u8)) -> ();
    fn __gruel_utf8_validate(buf: Ptr(u8), len: usize) -> u8;
}
```

The `gruel_runtime` library is the static archive `libgruel_runtime.a` that's already linked into every Gruel executable. ADR-0085's library-set walker emits the corresponding `-l<name>` (or, for the runtime archive, the path-based link line the compiler already manages today — same as before this ADR).

Per ADR-0085 dedup rules, user code is free to declare its own `link_extern("c") { fn malloc(...); }` block; the symbols dedupe against the prelude's declarations. Nothing about the binding mechanism is implicit or compiler-special.

### Pthread library naming

The `@thread_spawn`/`@thread_join` codegen calls a prelude wrapper that uses `pthread_create` / `pthread_join`. Where those symbols live varies by target:

- **glibc Linux**: a separate `libpthread.so` (modern glibc unifies into libc.so.6 with weak symbols, but `-lpthread` is the convention).
- **musl Linux**: pthread is part of libc; no separate library.
- **Mach-O Darwin**: pthread is part of libSystem; no separate library.

The prelude's `link_extern` block for pthread is target-conditional — on glibc Linux it's `link_extern("pthread") { fn pthread_create(...); … }`; on musl and Darwin the same symbols are declared in the `link_extern("c")` block (or omitted, with the linker resolving via the always-on libc / libSystem). Until Gruel has conditional-compilation primitives, the prelude generator emits the right shape based on the compilation target.

This is the only target-conditional bit in the prelude. Everything else (libc, runtime archive) is target-uniform.

### `@`-syntax removal

Removing `@panic("msg")` syntax in favor of `panic("msg")` is a user-visible change. User code in spec tests, runtime examples, and any in-tree Gruel sources is updated as part of Phase 2/3. Because Gruel is pre-1.0 and pre-stable, there's no deprecation window: the old syntax stops parsing the moment the corresponding intrinsic row is removed.

For migration ease in subsequent phases, the order is panic → dbg lowered targets → IO/algorithmic → threads → heap/exit. The runtime-error paths (which are compiler-emitted, not user-written) shift first; user-facing `@panic` callers shift in Phase 2; the heap/exit `@alloc`/`@free`/`@realloc`/`@exit` family (which is rarely written by user code outside `checked` blocks) shifts last. `@spawn`'s migration in Phase 5 is the highest-friction user-visible shift since it replaces the bare `@spawn(fn, arg)` form with `JoinHandle(R).spawn(fn, arg)` — but in-tree usage today is small and bounded.

### Why this isn't a preview-feature gated rollout

Preview features (ADR-0005) gate the *addition* of new syntax. This ADR is the *removal* of `@`-prefixed names in favor of bare-fn-name names; the prelude fns are additions but they're idiomatic Gruel fns, not new syntax. There's nothing to gate. The migration is a single user-visible cliff: `@panic` stops parsing the moment Phase 2 lands. Existing in-tree sources are updated in the same commit.

### Diagnostics

No new permanent diagnostics. Phases 2 / 3 / 4 / 5 / 6 each may temporarily fire a `IntrinsicMigrated { old: "@foo", new: "foo" }` error during the migration window to help any out-of-tree callers find the rename; the diagnostic retires when the phase completes.

## Implementation Phases

### Phase 1: Prelude crate + linkage scaffolding

- [ ] Add a `prelude` module (or set of modules) in `gruel-builtins`.
- [ ] Establish injection: the compiler walks prelude fns the same way it walks `String`/`Vec(T)` synthetic structs today (see ADR-0020).
- [ ] Add the prelude's `link_extern("c") { … }` block (libc bindings used by every Phase 2–5 migration: `write`, `read`, `exit`, `malloc`, `free`, `realloc`, `memcmp`). Ordinary source `link_extern`; no compiler-side implicit-link machinery.
- [ ] Add the prelude's `link_extern("gruel_runtime") { … }` block declaring the surviving `__gruel_*` symbols (`__gruel_parse_*`, `__gruel_random_*`, `__gruel_utf8_*`, `__gruel_cstr_to_vec`) and, until Phase 3 retires them, the `__gruel_dbg_*` symbols. Verify the library-set walker emits the runtime archive on the link line (it already does today; the prelude declaration is what makes the binding source-visible rather than codegen-magic).
- [ ] Pthread library-naming: emit the prelude's pthread block as `link_extern("pthread")` on glibc Linux, fold the symbols into `link_extern("c")` on musl and Darwin. Implemented as part of the prelude-emission step that runs per target.
- [ ] Verify the prelude fn name namespace doesn't clash with anything in existing spec tests.

### Phase 2: Migrate the panic family

- [ ] Add prelude fns: `panic(msg)`, `panic_no_msg`, `panic_div_by_zero`, `panic_intcast_overflow`, `panic_bounds_check`, `panic_float_to_int_overflow`, `panic_vec_dispose`.
- [ ] Codegen: rewrite compiler-emitted panic call sites (overflow checks, bounds checks, drop-glue panics, vec-dispose panic) to call the new prelude fns directly.
- [ ] Sema/codegen: remove `IntrinsicId::Panic` and the associated arms.
- [ ] Lexer/parser: `@panic` stops being a recognised intrinsic name.
- [ ] Update in-tree `@panic("...")` callers (spec tests, examples) to `panic("...")`.
- [ ] Runtime: delete `__gruel_panic`, `__gruel_panic_no_msg`, `__gruel_vec_dispose_panic`, `__gruel_div_by_zero`, `__gruel_intcast_overflow`, `__gruel_bounds_check`, `__gruel_float_to_int_overflow` from `gruel-runtime/src/error.rs`. The `#[panic_handler]` in `entry.rs` keeps using `platform::exit` directly.

### Phase 3: Migrate `@dbg`'s lowered targets

- [ ] Add prelude fns: `dbg_i64(x)`, `dbg_u64(x)`, `dbg_bool(b)`, `dbg_str(s)`, `_noln` variants, `dbg_space()`, `dbg_newline()`.
- [ ] `@dbg`'s codegen arm rewrites to call the new prelude fns per-argument.
- [ ] `@dbg` *itself* stays as an intrinsic (compile-time type dispatch).
- [ ] Runtime: delete `__gruel_dbg_*` from `gruel-runtime/src/debug.rs`.

### Phase 4: Migrate IO + algorithmic wrappers

- [ ] Add prelude fns: `read_line`, `parse_i32`, `parse_i64`, `parse_u32`, `parse_u64`, `random_u32`, `random_u64`, `cstr_to_vec`, `utf8_validate`, `memcmp`.
- [ ] Sema/codegen: remove the corresponding `IntrinsicId::*` variants and arms.
- [ ] Lexer/parser: the `@`-prefixed names stop parsing.
- [ ] Update in-tree callers (mostly Vec methods that use `@cstr_to_vec`/`@utf8_validate`/`@memcmp`, plus spec tests for parse/random/read_line).
- [ ] Runtime: keep `__gruel_parse_*`, `__gruel_random_*`, `__gruel_cstr_to_vec`, `__gruel_utf8_validate` (the prelude fns wrap them). Drop `__gruel_memcmp` if ADR-0086 hasn't already (it removed it as part of the direct-libc cleanup; verify and skip).
- [ ] Runtime: delete `__gruel_read_line` after the corresponding prelude fn is in.

### Phase 5: Migrate threads

- [ ] Add low-level prelude fns: `thread_spawn(thunk: MutPtr(c_void), arg: MutPtr(c_void), ret_size: usize) -> MutPtr(c_void)` and `thread_join(handle: MutPtr(c_void), ret_out: MutPtr(c_void), ret_size: usize)`. Bodies bind `pthread_create` and `pthread_join` via the prelude's `link_extern` blocks (libc or pthread per the target-conditional rule).
- [ ] Add the higher-level `JoinHandle(R)` prelude struct (see §"Thread API shape") with `JoinHandle(R).spawn(comptime A, comptime F, f: F, arg: A) -> JoinHandle(R)` and `join(self) -> R` methods. The `spawn` method synthesises the `@mark(c)` marshaling thunk per `(A, R, F)` triple via comptime-generic monomorphisation, calls `thread_spawn` with the synthesised thunk and a boxed arg, wraps the resulting handle in the linear `JoinHandle`. The `join` method calls `thread_join` and unboxes the return.
- [ ] Compile-time validation that ADR-0084 used to live in `@spawn`'s sema arm (arity = 1; arg ≥ Send and not Linear/ref; return ≥ Send) moves into the `JoinHandle.spawn` method body as comptime asserts over the parameter types' markers. Same checks, different host.
- [ ] Sema/codegen: remove `IntrinsicId::ThreadSpawn` (formerly `@spawn`) and `IntrinsicId::ThreadJoin`. Lexer/parser: the `@spawn` and `@thread_join` names stop parsing.
- [ ] Update in-tree `@spawn(worker, arg)` callers to `JoinHandle(WorkerResult).spawn(worker, arg)` (or a `spawn(worker, arg)` free-fn prelude shorthand if Phase 5 adds one — see Open Question 2).
- [ ] Runtime: delete `__gruel_thread_spawn`, `__gruel_thread_join`.

### Phase 6: Migrate heap + exit

- [ ] Add prelude fns: `alloc`, `free`, `realloc`, `exit`. ADR-0086 already supports the libc bindings via the prelude's `link_extern("c")` block (added in Phase 1).
- [ ] Codegen: rewrite Vec(T) push/reserve/clone to call the new prelude fns; rewrite main-return to call `exit`.
- [ ] Sema/codegen: remove `IntrinsicId::Alloc`, `IntrinsicId::Free`, `IntrinsicId::Realloc`, `IntrinsicId::Exit`.
- [ ] Lexer/parser: the `@`-prefixed names stop parsing.
- [ ] Update in-tree user-facing `@alloc`/`@free`/`@realloc` callers (mostly inside `checked` blocks in tests) to bare fn names.
- [ ] Runtime: delete `__gruel_alloc`, `__gruel_free`, `__gruel_realloc`, `__gruel_exit` (the trivially-libc-shaped shims that an earlier draft of ADR-0086 was going to retire and that this phase finally does).

### Phase 7: Stabilise

- [ ] Confirm the intrinsics registry contains only rows that do real compiler magic — `IntrinsicId::Dbg` is the last libc-wrapper-style row; the rest of the registry (`Assert`, `CompileError`, `Cast`, `SizeOf`, `AlignOf`, `TypeName`, `TypeInfo`, `Ownership`, `ThreadSafety`, `Implements`, `Field`, `Import`, `EmbedFile`, `TargetArch`, `TargetOs`, the `Ptr*` family, `Range`, the `Slice*` family, `VecLiteral`/`VecRepeat`/`PartsToVec`, `Uninit`/`Finalize`/`FieldSet`/`VariantUninit`/`VariantField`, `Syscall`, `TestPreviewGate`) is compile-time reflection / codegen-emitted lang-item lowering / pointer ops, all of which earned their place by doing real compiler work.
- [ ] Document the new "intrinsics carry compiler magic, not transport" rule in the `gruel-intrinsics` crate's module docs and in ADR-0050's open-questions section (cross-ref).
- [ ] ADR status → `implemented`.

## Consequences

### Positive

- The intrinsics registry contracts to its design intent: things that need compiler magic. Adding a new libc binding is no longer a registry change; it's a prelude fn with a `link_extern` line.
- One linkage mechanism. The prelude binds libc and the runtime archive through ordinary source `link_extern` blocks; the library-set walker from ADR-0085 handles them with no compiler-introduced "implicit link" parallel mechanism.
- The `@`-prefix becomes a sharper signal — it reliably means "compiler magic happens here", not "this happens to be in the runtime".
- The Rust runtime shrinks substantially. After this ADR, the surviving symbols are the algorithmic helpers (`parse_*`, `random_*`, `utf8_*`, `cstr_to_vec`) plus platform entry points and the `#[panic_handler]`. The `debug.rs`, `error.rs`, `io.rs`, `thread.rs`, `heap.rs` modules collapse or shrink to nothing.
- User code that wants debugging output / panic / parse / etc. uses idiomatic Gruel fn calls rather than a special syntax that doesn't exist anywhere else in the language.
- The FFI surface from ADR-0085 / ADR-0086 stops being two-tier (runtime symbols vs user FFI); after this ADR there's just one mechanism — `link_extern` — and the runtime archive is one more `link_extern` target alongside libc.

### Negative

- One user-visible cliff: `@panic`/`@read_line`/`@parse_*`/`@spawn`/etc. stop parsing the moment each migration phase lands. Pre-1.0 Gruel has no compatibility commitment, so this is acceptable, but in-tree callers need to update in lockstep with each phase. The `@spawn` → `JoinHandle(R).spawn` rewrite in Phase 5 is the most invasive at the call-site level (every spawn point gets `JoinHandle(R).` prepended), though Open Question 2 floats a `spawn(...)` free-fn shorthand to recover the old shape.
- The prelude grows a `link_extern("gruel_runtime")` block. The runtime archive was already implicitly linked today; making the binding source-visible (via the prelude block) is honest, but means the prelude is now load-bearing for the runtime contract.
- Pthread library-naming is target-conditional in the prelude emission — small but real platform-aware complexity.
- Prelude fn name pollution. Names like `panic`, `read_line`, `alloc`, `free`, `realloc`, `exit`, `memcmp`, `thread_spawn`, `thread_join` are now globally bound. User code that wants to define a fn with one of those names has to pick a different name or wait for the module system to land. Pre-1.0 Gruel's user surface is small enough that this is unlikely to bite anyone.
- The thunk-synthesis pass inside `JoinHandle.spawn` is new compiler work — comptime-generic monomorphisation extended to emit `@mark(c)` synthetic fns. ADR-0055 Phase 4 already does the comptime-generic part; the `@mark(c)` extension is the new piece. Open Question 2 tracks whether that extension fits this ADR's scope or wants its own micro-ADR.
- `@dbg` stays as an intrinsic — explicit deferral to a future "comptime kind-dispatch" ADR. The registry isn't *empty* of libc-wrapper-style rows; it just doesn't grow with them anymore.

### Neutral

- The Rust-runtime archive stops being the source of truth for "what symbols the compiler relies on" — that truth moves to the prelude. The prelude is the new ground truth.
- No runtime-archive size change for the surviving algorithmic helpers; only the `__gruel_*` symbols whose bodies were libc shims disappear.
- The `@`-prefixed intrinsic surface gets smaller. After this ADR, the only libc-wrapper-style `@`-prefixed name a user writes is `@dbg`; everything else with the `@` is a compile-time reflection / pointer / slice / lang-item codegen intrinsic that does real compiler work (`@size_of`, `@type_info`, `@ptr_read`, `@import`, `@target_os`, `@vec_literal`, `@uninit`, `@field`, etc.).

## Open Questions

1. **`@dbg` migration timing.** This ADR defers `@dbg` itself to a future "comptime kind-dispatch" ADR. Is there an interim shape that drops the variadic surface in favor of explicit per-type fns (`dbg_i64(x); dbg_newline()`)? Recommend no — variadic `@dbg` is too useful to break without a replacement.
2. **`@mark(c)` on comptime-synthesised fns.** The `JoinHandle.spawn` method synthesises a thunk per-monomorphisation; the synthesised fn needs the `@mark(c)` marker so the resulting symbol has C ABI. ADR-0055 Phase 4 already handles comptime-generic fn monomorphisation; extending it to allow `@mark(c)` on the synthesised fn is one focused piece of compiler work. Is it scope-creep for this ADR, or does it fit cleanly in Phase 5? Recommend: try to fit, defer to a follow-up micro-ADR if it grows.
3. **`spawn(...)` free-fn shorthand.** With `comptime R = @return_type(F)` inference, a free-fn `spawn(f, arg)` in the prelude could recover the old `@spawn(f, arg)` ergonomics. Is the return-type-inference primitive present? If yes, ship the shorthand alongside `JoinHandle.spawn` in Phase 5.
4. **Prelude fn name conflicts in user code.** Pre-module-system, names like `panic` and `exit` are global. Should the prelude fns be addressable under a longer-qualified name as well (e.g. `prelude::panic`)? Recommend yes once the module system arrives; pre-module-system the shorter names are the only names.
5. **Migration order for in-tree callers.** Each phase touches in-tree spec tests / examples; should each phase be one large PR that updates everything atomically, or can it be staged with a temporary `@<name>` alias that calls the prelude fn? Recommend atomic — pre-1.0 lets us avoid the alias complexity.
6. **What happens to the `gruel-intrinsics` crate after the contraction?** It keeps a large slate of *compiler-magic* rows (compile-time reflection like `@size_of`/`@type_info`/`@target_os`, codegen-emitted lang-item lowerings like `@vec_literal`/`@uninit`/`@field`, raw-pointer ops like `@ptr_read`/`@ptr_offset`, slice ops like `@slice_index_read`, plus `@assert`/`@compile_error`/`@cast`/`@import`/`@embed_file`/`@syscall`/`@dbg`). The libc-wrapper subset leaves; everything else stays. The crate is still load-bearing — it hosts the closed `IntrinsicId` enum and the exhaustive registry that sema and codegen pattern-match against. Reorganisation of the surviving rows (whether they should be one crate, multiple per-category crates, or folded into `gruel-air`) is a separate question; flagged as Future Work below.

## Future Work

- **Comptime kind-dispatch for `@dbg`**, allowing it to leave the intrinsics registry and become a generic `dbg<T>(x: T)` prelude fn — the last libc-wrapper-style row clears at that point.
- **Typed extern fn pointer types** — strictly stronger than the `MutPtr(c_void)` transport this ADR uses for the thread thunk, would let `JoinHandle.spawn`'s synthesised thunk be a typed value rather than a `c_void`-typed pointer at the prelude API boundary. Type safety for fn-pointer FFI more generally.
- **Re-implementing the surviving algorithmic helpers** (`parse_*`, `random_*`, `utf8_*`) in Gruel once the stdlib grows the necessary primitives — orthogonal to ADR-0087's intrinsic-registry cleanup but related: it eventually deletes the prelude's `link_extern("gruel_runtime")` block entirely (the runtime archive itself goes away once nothing in the prelude wraps it).
- **Reorganisation of the surviving `gruel-intrinsics` crate.** After this ADR the crate hosts only compiler-magic rows. Whether they stay together, split per-category (reflection, codegen-emitted lang-items, pointer ops, slice ops), or fold into `gruel-air` is a packaging question separate from the migration this ADR performs.
- **Module-system migration** when ADR-TBD lands: the prelude moves from compiler-injected names to `use std::prelude::*`-style auto-import.

## References

- [ADR-0005: Preview Features](0005-preview-features.md) — explicitly *not* used here; this is removal of syntax, not addition.
- [ADR-0020: Built-in Types as Structs](0020-builtin-types-as-structs.md) — the injection mechanism that prelude fns reuse.
- [ADR-0050: Intrinsics Crate](0050-intrinsics-crate.md) — the registry whose libc-wrapper rows leave here.
- [ADR-0055: Comptime type-arg inference](0055-comptime-type-arg-inference.md) — the Phase 4 monomorphisation pass that `JoinHandle.spawn`'s thunk synthesis extends.
- [ADR-0084: Send/Sync Markers](0084-send-sync-markers.md) — the `Send`/`Sync` checks that used to live in `@spawn`'s sema arm and now live in `JoinHandle.spawn`'s comptime asserts.
- [ADR-0085: C foreign function interface](0085-c-ffi.md) — `link_extern` is what makes the migration possible.
- [ADR-0086: C FFI extensions](0086-c-ffi-extensions.md) — direct parent. The `c_void` + `@mark(c) fn`-to-`MutPtr(c_void)` cast it ships is what unblocks `@thread_spawn`/`@thread_join` retirement.

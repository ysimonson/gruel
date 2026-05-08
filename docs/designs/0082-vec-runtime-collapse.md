---
id: 0082
title: Vec Runtime Collapse onto Gruel Primitives
status: proposal
tags: [stdlib, collections, runtime, vec, intrinsics]
feature-flag: vec_runtime_collapse
created: 2026-05-07
accepted:
implemented:
spec-sections: ["7.3"]
superseded-by:
---

# ADR-0082: Vec Runtime Collapse onto Gruel Primitives

## Status

Proposal

## Summary

Migrate the per-method codegen-inline LLVM lowering of `Vec(T)` (~900 LOC across `gruel-codegen-llvm/src/codegen.rs:4140–5418`) to Gruel-level method bodies declared in `prelude/vec.gruel`. The mechanism mirrors the path Option / Result already use: a `pub fn Vec(comptime T: type) -> type { struct { ... methods ... } }` comptime function returns an anonymous struct, and sema instantiates per-`T`. To unblock the move, three new Gruel-callable intrinsics are added — `@alloc(size: usize, align: usize) -> MutPtr(u8)`, `@realloc(p, old_size, new_size, align) -> MutPtr(u8)`, and `@free(p, size, align)` — exposing the existing `__gruel_alloc` / `__gruel_realloc` / `__gruel_free` runtime symbols through the intrinsic registry, gated to `checked` blocks. Per-element drop in `Vec::drop` is expressed via raw pointer reads (`self.ptr.add(i).read()` lets the read value fall out of scope, invoking `T`'s drop) — no new `@drop(value)` intrinsic is needed. The `TypeKind::Vec(TypeId)` marker stays, preserving sema's place-grammar (`&v[..]`) and indexing (`v[i]`); method dispatch routes to the prelude declaration's method list rather than to `vec_*` intrinsics. The 19 Vec-related `IntrinsicId` variants shrink to 2 (17 retire outright; `@vec(...)` and `@vec_repeat(v, n)` keep their variadic registry surface but their codegen lowering retires — they desugar to `Vec::with_capacity(n) + push(...)` chains via the prelude methods).

**Slice(T) / MutSlice(T) are out of scope.** They are scope-bound second-class types (per ADR-0064) whose non-escape guarantees cannot be expressed in a regular Gruel struct declaration; the borrow-checker enforces scope-restriction off the `TypeKind::Slice(_) | TypeKind::MutSlice(_)` marker check at `crates/gruel-air/src/sema/analysis.rs:3319`, and a prelude-declared `pub fn Slice(comptime T: type) -> type { struct { ... } }` would produce a regular first-class struct that loses every guarantee. Migrating Slice requires a "second-class struct" mechanism (a `Borrowed` interface, an attribute on prelude struct declarations, or method-extension blocks against the existing TypeKind marker — see Future Work). Until then, the Slice / MutSlice codegen-inline path stays unchanged. Vec methods that take Slice arguments (`contains`, `starts_with`, `ends_with`, `concat`, `extend_from_slice`) reference `Slice(T)` only as a parameter type; the prelude struct can name the type since it exists in the type system, but never constructs or returns one.

**LOC impact.** Roughly 900 LOC out of `gruel-codegen-llvm/src/codegen.rs` (per-method `translate_vec_*` functions) and ~500 LOC of Vec sema dispatch in `crates/gruel-air/src/sema/vec_methods.rs` (which shrinks from ~658 to ~150). Net Rust LOC retired: ~1400. New Gruel LOC added: ~150 (Vec methods in `prelude/vec.gruel`). New Rust LOC added: ~80 (three new memory intrinsics + their sema and codegen wiring).

## Status of dependencies

This ADR builds on:

- **ADR-0066** (Vec(T)) — the substrate being migrated.
- **ADR-0064** (Slices) — defines the Slice / MutSlice second-class types this ADR explicitly leaves in place; their migration is queued as Future Work.
- **ADR-0081** (String runtime collapse) — establishes the playbook of moving a built-in type's method bodies to the prelude. ADR-0081 ships first because it has no dependency on Vec changes; ADR-0082 takes longer to land safely. ADR-0081 also adds seven new Vec(T) byte-comparison and search methods (`eq`, `cmp`, `contains`, `starts_with`, `ends_with`, `concat`, `extend_from_slice`) — those land as codegen-inline LLVM in 0081 and are migrated to Gruel here.
- **ADR-0078** (Stdlib MVP) — established the prelude as a directory and the comptime-function-returning-type pattern (Option, Result).
- **ADR-0028** (Unchecked code and raw pointers) — the `checked` block mechanism that gates the new memory intrinsics.

## Context

### Where things sit today

- **Vec(T)** is a `TypeKind::Vec(TypeId)` variant in the type pool, registered via `BuiltinTypeConstructorKind::Vec` in `gruel-builtins/src/lib.rs:752–761`. It is *not* a `BuiltinTypeDef` — there is no synthetic struct injection. Method dispatch goes through `crates/gruel-air/src/sema/vec_methods.rs:87–658` (~658 LOC), which special-cases on method name and emits `vec_*` AIR intrinsic nodes. Codegen at `crates/gruel-codegen-llvm/src/codegen.rs:4140–5418` matches on the intrinsic id and emits inline LLVM (~900 LOC across 16 methods). The grow / drop / clone paths call `__gruel_alloc` / `__gruel_realloc` / `__gruel_free` runtime symbols directly via inline LLVM (e.g., line 5237 for free).
- **Slice(T) / MutSlice(T)** are scope-bound second-class types (ADR-0064): they cannot be stored in struct fields, returned as a top-level return type, escape a borrow scope, or be elements of a `Vec(Slice(T))`. The borrow-checker enforces this off the `TypeKind::Slice(_) | TypeKind::MutSlice(_)` marker check at `crates/gruel-air/src/sema/analysis.rs:3319`. Method dispatch is in `crates/gruel-air/src/sema/pointer_ops.rs::dispatch_slice_method_call` (~200 LOC); codegen translates `slice_*` intrinsics to inline LLVM (~150 LOC). Slice migration to a prelude declaration is **out of scope for this ADR** — see Future Work.
- **`@alloc` / `@realloc` / `@free` are not Gruel-callable.** They exist as runtime FFI symbols (`__gruel_alloc` in `gruel-runtime/src/string.rs:88`, `__gruel_realloc` at line 100, `__gruel_free` at line 94 — to be relocated when `string.rs` is renamed per ADR-0081). User code cannot call them. The codegen calls them directly during Vec method translation.
- **`@size_of(T)` / `@align_of(T)`** exist as Gruel-callable type-intrinsics (`gruel-intrinsics/src/lib.rs:48–49`, `IntrinsicKind::Type`). They evaluate to `usize` at compile time and are usable inside method bodies.
- **Pointer ops** (`p.read()`, `p.write(v)`, `p.add(n)`, `p.offset(n)`) exist as `checked`-block-gated intrinsics. Reading a value out of `MutPtr(T)` produces a value of type `T` whose ownership transfers to the call site; falling out of scope runs `T`'s drop. This is the load-bearing primitive for Vec's per-element drop loop.
- **The existing `Vec(T)` drop path** is synthesized by codegen in `gruel-codegen-llvm/src/codegen.rs::emit_vec_drop_loop:5331–5376`. It walks `[0..len]`, calls each element's drop function (recognized via `drop_names::type_needs_drop` — line 5226), then frees the buffer. After this ADR, the same logic is expressed in Gruel inside the prelude `Vec::drop` method body.
- **Indexing (`v[i]`)** dispatches via `try_analyze_vec_index_read` / `try_analyze_vec_index_write` in `vec_methods.rs:490–544`, which special-cases `TypeKind::Vec(_)` and emits `vec_index_read` / `vec_index_write` intrinsics. Range subscripts (`&v[..]`, `&v[a..b]`) go through the place-grammar (`crates/gruel-air/src/sema/analysis.rs:2330` — `MakeSlice` IR node).
- **Slice scope-restriction** is enforced in `crates/gruel-air/src/sema/analysis.rs:3319` keyed off `TypeKind::Slice(_)` / `TypeKind::MutSlice(_)`. The borrow-checker rejects slices escaping their borrow scope.

### What's missing

1. **Gruel-callable memory intrinsics.** Without `@alloc` / `@realloc` / `@free`, a Gruel-level Vec body cannot allocate or free its buffer; the methods would have nothing to delegate to. The runtime symbols already exist — they just need a thin layer in the intrinsics registry exposing them as Gruel-callable.
2. **A prelude-resident Vec declaration.** Today Vec's per-T monomorphization is opaque to the prelude — it's all codegen-internal. Migrating method bodies requires Vec to be declared in `prelude/vec.gruel` as a comptime function returning an anonymous struct, with each method body written in Gruel.
3. **Sema dispatch routing.** `dispatch_vec_method_call` currently emits `vec_*` intrinsics; after migration, it routes to the prelude declaration's instantiated methods. This is the load-bearing change: the existing `TypeKind::Vec(T)` recognition stays (so place-grammar and borrow-checker keep working), but the method-body lookup goes through the prelude.

### What this ADR does *not* attempt

- **Migrate Slice(T) / MutSlice(T) to a prelude declaration.** Slices are scope-bound second-class types whose non-escape guarantees are enforced by the compiler off `TypeKind::Slice(_) | TypeKind::MutSlice(_)` marker checks; a prelude declaration like `pub fn Slice(comptime T: type) -> type { struct { ptr, len } }` would produce a regular first-class struct that loses every guarantee (slices could be returned, stored in fields, escape borrow scopes). Migrating Slice requires a "second-class struct" mechanism — a `Borrowed` interface or attribute that lets a prelude struct opt into scope-restriction, or method-extension blocks (`impl Slice(T) { ... }`) targeting the existing TypeKind marker. Both are real language work; flagged in Future Work as the natural follow-up.
- **Replace `TypeKind::Vec(_)` with a regular struct type.** It stays as a marker variant. Place-grammar (`&v[..]`) and indexing (`v[i]`) depend on it. Generalizing these to interface-driven dispatch (e.g., an `Index` / `IndexMut` interface analogous to ADR-0078's Eq / Ord) is real but separable future work.
- **A general `@drop(value)` intrinsic.** The Vec drop body can express per-element drop via `let _ = self.ptr.add(i).read();` — the read produces an owned `T` whose drop runs at scope exit. No new primitive needed.
- **Comptime-generic struct syntax** (`pub struct Vec(comptime T: type) { ... }`). The comptime-function-returning-anonymous-struct pattern (Option, Result, this ADR's Vec) handles the same use case without a syntactic addition.
- **Stabilize the new memory intrinsics for general use.** `@alloc` / `@realloc` / `@free` ship behind `checked` blocks and the `vec_runtime_collapse` preview gate during Phases 1–4. Whether they stay `checked`-gated indefinitely (as raw memory primitives) or graduate to ungated use (requiring a clear safety story) is a separate decision flagged in Open Questions.
- **Allocator parameterization** (`Vec(T, A)`). Out of scope; ADR-0066 future work; depends on an `Allocator` interface.
- **Linear-element support for Vec(T:Linear).** Same as ADR-0066: deferred. The Gruel-level body must still reject linear `T` at sema for the same reasons (implicit drops in the drop loop violate linearity).
- **Spec rewrites.** Spec section 7.3 (Vec) needs an informative note pointing at the prelude declaration; no normative paragraph changes — observable semantics are unchanged.

### Why now

ADR-0081 lands first and is independent. Once it does, the playbook is established: a built-in *first-class* type's method bodies move to a prelude declaration, with the type identity preserved via existing recognition mechanisms. Vec is the largest remaining customer of codegen-inline lowering — ~900 LOC of `translate_vec_*` functions in `codegen.rs`. Slice is the next-largest, but it doesn't fit the playbook: as a second-class type, it can't be expressed as a regular Gruel struct without a new mechanism. Tackling it now would either bundle the second-class-struct design into this ADR (mega-landing risk) or land Slice in a half-migrated state. Better to ship Vec cleanly here and queue Slice for a follow-up ADR once the second-class mechanism exists. The maintenance hazard today: every new Vec method (even something as trivial as `last() -> Option(T)` from ADR-0066's "Future Work") requires a coordinated edit across `gruel-intrinsics`, sema dispatch, and codegen lowering. After this ADR, it's "edit `prelude/vec.gruel`."

## Decision

### 1. Three new Gruel-callable memory intrinsics

Add to `crates/gruel-intrinsics/src/lib.rs`, all `IntrinsicKind::Expr`, all `checked`-block-gated, all preview-gated to `vec_runtime_collapse` during Phases 1–4:

```
@alloc(size: usize, align: usize) -> MutPtr(u8)
@realloc(p: MutPtr(u8), old_size: usize, new_size: usize, align: usize) -> MutPtr(u8)
@free(p: MutPtr(u8), size: usize, align: usize)
```

Codegen lowering: direct call to the existing `__gruel_alloc` / `__gruel_realloc` / `__gruel_free` runtime symbols, passing through the byte-size and alignment arguments. ~30 LOC of codegen + ~50 LOC of intrinsic registration / sema arity-and-type checks.

The byte-level (untyped) shape matches the runtime symbols exactly. Vec body code does its own size math via `n * @size_of(T)` and casts the returned `MutPtr(u8)` to `MutPtr(T)` via the existing `@ptr_cast` intrinsic (or whatever the current pointer-cast mechanism is — verify in Phase 1). Typed convenience wrappers (`@alloc_n(T, n)`) are deferred to follow-up sugar; the byte form is the load-bearing primitive.

The `checked`-block gate is conservative. Memory ops can leak, double-free, or alias; gating to `checked` follows ADR-0028's posture for raw-pointer primitives. The Vec methods that use these intrinsics carry the `checked` block internally — call sites of `v.push(x)` from user code do not need a `checked` block, just as `s.terminated_ptr()` on a String today wraps its `checked` requirement internally.

### 2. Prelude `Vec(T)` declaration

New file `prelude/vec.gruel` (or alternatively `prelude/collections/vec.gruel` if a directory carve-out is preferred — see Open Questions §1). Skeleton (full method list in §3):

```gruel
// ADR-0066 + ADR-0082: owned, growable vector. Layout { ptr, len, cap }.
// Allocations come from @alloc/@realloc/@free; per-element drop is
// expressed via raw pointer reads (the read'd value falls out of scope
// and runs T's drop).
pub fn Vec(comptime T: type) -> type {
    struct {
        ptr: MutPtr(T),
        len: usize,
        cap: usize,

        pub fn new() -> Self {
            Self { ptr: checked { @null_ptr_mut(T) }, len: 0, cap: 0 }
        }

        pub fn with_capacity(n: usize) -> Self {
            if n == 0 {
                return Self::new();
            }
            let p_u8: MutPtr(u8) = checked {
                @alloc(n * @size_of(T), @align_of(T))
            };
            let p: MutPtr(T) = checked { @ptr_cast(MutPtr(T), p_u8) };
            Self { ptr: p, len: 0, cap: n }
        }

        pub fn len(self: Ref(Self)) -> usize { self.len }
        pub fn capacity(self: Ref(Self)) -> usize { self.cap }
        pub fn is_empty(self: Ref(Self)) -> bool { self.len == 0 }

        pub fn push(self: MutRef(Self), value: T) {
            if self.len == self.cap {
                let new_cap: usize = if self.cap == 0 { 4 } else { self.cap * 2 };
                let old_bytes: usize = self.cap * @size_of(T);
                let new_bytes: usize = new_cap * @size_of(T);
                let p_u8: MutPtr(u8) = checked {
                    let raw = @ptr_cast(MutPtr(u8), self.ptr);
                    @realloc(raw, old_bytes, new_bytes, @align_of(T))
                };
                self.ptr = checked { @ptr_cast(MutPtr(T), p_u8) };
                self.cap = new_cap;
            }
            checked { self.ptr.add(self.len).write(value) };
            self.len = self.len + 1;
        }

        pub fn pop(self: MutRef(Self)) -> Option(T) {
            if self.len == 0 {
                return Option(T)::None;
            }
            self.len = self.len - 1;
            let v: T = checked { self.ptr.add(self.len).read() };
            Option(T)::Some(v)
        }

        // Drop body: read each element out (its drop runs at scope exit),
        // then free the buffer if it was allocated.
        pub fn drop(self) {
            var i: usize = 0;
            while i < self.len {
                // The read produces an owned T whose drop runs when this
                // binding falls out of scope at the end of the loop body.
                let _: T = checked { self.ptr.add(i).read() };
                i = i + 1;
            }
            if self.cap > 0 {
                checked {
                    let raw = @ptr_cast(MutPtr(u8), self.ptr);
                    @free(raw, self.cap * @size_of(T), @align_of(T))
                };
            }
        }

        // ... remainder of methods (clear, reserve, clone, eq, cmp,
        // contains, starts_with, ends_with, concat, extend_from_slice,
        // index_read, index_write, ptr, ptr_mut, terminated_ptr, dispose) ...
    }
}
```

Key body-level techniques:

- **Raw pointer offset reads/writes** for indexing and per-element copies: `self.ptr.add(i).read()` / `.write(v)`. Used in `push`, `pop`, `clone`, `drop`, `index_read`, `index_write`, the byte-search methods.
- **`@realloc` for the grow path** in `push` and `reserve`. The doubling-capacity policy that `__gruel_vec_grow` historically encapsulated lives in Gruel now; the policy is editable in the prelude file.
- **Drop loop via scope-exit drop** in `drop()`. No `@drop` primitive needed.
- **`checked` blocks wrap each individual unchecked op** — `@ptr_cast`, `@alloc`, `@realloc`, `@free`, `.add(n).read/write`. The Gruel-level body absorbs the `checked` requirement; user call sites of `v.push(x)` see no `checked` requirement.

### 3. Vec method surface (full)

The prelude declaration carries every method `Vec(T)` has today plus the seven added in ADR-0081 Phase 1. All bodies are Gruel-level compositions over the primitives above:

| Method | Constraint | Body summary |
|---|---|---|
| `new()` | none | zero-init aggregate |
| `with_capacity(n)` | none | `@alloc` + return `{p, 0, n}` |
| `len`, `capacity`, `is_empty` | none | direct field access |
| `push(value: T)` | none | grow if `len == cap`, write, inc len |
| `pop() -> Option(T)` | none | dec len + read out + return `Some` (or `None` if empty) |
| `clear()` | none | drop-loop + len = 0 (cap unchanged) |
| `reserve(n: usize)` | none | grow-to-additional |
| `clone() -> Self` | T: Copy (v1) | `@alloc` + memcpy of `len * sizeof(T)` |
| `index_read(i: usize) -> T` | T: Copy | bounds check + `ptr.add(i).read()` |
| `index_write(i: usize, v: T)` | none | bounds check + `ptr.add(i).write(v)` |
| `ptr() -> Ptr(T)` | checked | `@ptr_cast(Ptr(T), self.ptr)` |
| `ptr_mut() -> MutPtr(T)` | checked | `self.ptr` |
| `terminated_ptr(s: T) -> Ptr(T)` | T: Copy, checked | grow if `cap == len`, write `s` at `ptr[len]`, return ptr |
| `dispose()` | len == 0 (panics) | `@free` + drop self without running drop loop |
| `drop()` | none | element drop loop + `@free` |
| `eq(other: Ref(Self)) -> bool` | T: Copy | len equality + element-wise `==` |
| `cmp(other: Ref(Self)) -> Ordering` | T: Copy | element-wise lex compare |
| `contains(needle: Slice(T)) -> bool` | T: Copy | linear search via memcmp |
| `starts_with(prefix: Slice(T)) -> bool` | T: Copy | len check + memcmp |
| `ends_with(suffix: Slice(T)) -> bool` | T: Copy | len check + tail memcmp |
| `concat(other: Slice(T)) -> Self` | T: Copy | alloc + 2 element copies |
| `extend_from_slice(other: Slice(T))` | T: Copy | reserve + memcpy at `ptr+len` |

`@vec(...)` and `@vec_repeat(v, n)` stay as variadic intrinsics in the registry, but their codegen lowering retires. They desugar at sema time to `let v = Vec::with_capacity(N); v.push(a1); ... v.push(aN); v` (for `@vec`) or `let v = Vec::with_capacity(n); var i: usize = 0; while i < n { v.push(value.clone()); i = i + 1; } v` (for `@vec_repeat`, with the standard last-arg-moves optimization). The variadic surface stays in the parser/sema; the body is plain Gruel.

### 4. Sema dispatch routing (Phase 3)

`crates/gruel-air/src/sema/vec_methods.rs::dispatch_vec_method_call` (~658 LOC today) is rewritten:

- The 16+ method-name match arms that emit `vec_*` intrinsic nodes are replaced by a single lookup against the prelude `Vec(T)` declaration's method list.
- Each method call becomes a regular function call to the instantiated `Vec(T)::method` Gruel function, passing the receiver via the same receiver-mode machinery used for any user struct method.
- Indexing dispatch (`try_analyze_vec_index_read` / `try_analyze_vec_index_write`) routes to `Vec::index_read` / `Vec::index_write` calls.
- The final file is ~150 LOC: the `TypeKind::Vec(_)` recognition + place-grammar bridge + a dispatch helper. The 658 → 150 reduction is part of the Phase 3 LOC accounting.

`crates/gruel-air/src/sema/pointer_ops.rs::dispatch_slice_method_call` is **unchanged** — Slice migration is out of scope (see Future Work).

### 5. Codegen retirement (Phase 4)

Delete from `crates/gruel-codegen-llvm/src/codegen.rs`:

| Function (range) | LOC |
|---|---|
| `translate_vec_new` (4401–4409) | 8 |
| `translate_vec_with_capacity` (4410–4449) | 39 |
| `translate_vec_field_load` (4305–4324) | 19 |
| `translate_vec_push` (4450–4579) | 129 |
| `translate_vec_pop` (4582–4647) | 65 |
| `translate_vec_clear` (4650–4685) | 35 |
| `translate_vec_reserve` (4688–4760) | 72 |
| `translate_vec_index_read` (4763–4828) | 65 |
| `translate_vec_index_write` (4831–4896) | 65 |
| `translate_vec_terminated_ptr` (4897–5002) | 105 |
| `translate_vec_clone` (5003–5094) | 91 |
| `translate_vec_literal` (5095–5132) | 37 |
| `translate_vec_repeat` (5133–5193) | 60 |
| `translate_vec_dispose` (5250–5328) | 78 |
| `translate_parts_to_vec` (5411–5418) | 7 |
| `emit_vec_drop_loop` (5331–5376) | 45 |
| Vec dispatch table (4140–4192) | 53 |

**~970 LOC** of Vec codegen retires.

`__drop_Vec_T` per-T synthesis retires — Vec's drop is now a Gruel function that the standard drop dispatch (ADR-0010) calls at scope end. The compiler's drop-glue emission for a Vec-containing struct field calls the prelude `Vec::drop` instantiation, the same way it would for any user-declared affine struct with a `drop` method.

The seven new methods from ADR-0081 Phase 1 (`eq`, `cmp`, `contains`, `starts_with`, `ends_with`, `concat`, `extend_from_slice`) — those are codegen-inline LLVM under ADR-0081, also retire here. ~80 LOC.

The Slice codegen (`translate_slice_*`, ~150 LOC) is **unchanged** — Slice migration is out of scope.

### 6. The `IntrinsicId` cleanup

Retire from `crates/gruel-intrinsics/src/lib.rs`:

- 19 Vec-related variants: `VecNew`, `VecWithCapacity`, `VecLen`, `VecCapacity`, `VecIsEmpty`, `VecPush`, `VecPop`, `VecClear`, `VecReserve`, `VecIndexRead`, `VecIndexWrite`, `VecPtr`, `VecPtrMut`, `VecTerminatedPtr`, `VecClone`, `VecLiteral`, `VecRepeat`, `VecDispose`, `PartsToVec`. `VecLiteral` and `VecRepeat` stay (variadic surface is in the registry); net 17 retire, 2 stay.
- 7 Vec-byte-method variants from ADR-0081 Phase 1 (`VecEq`, `VecCmp`, `VecContains`, `VecStartsWith`, `VecEndsWith`, `VecConcat`, `VecExtendFromSlice`) — also retire.

Slice-related variants (`SliceLen`, `SliceIsEmpty`, `SliceIndexRead`, `SliceIndexWrite`, `SlicePtr`, `SlicePtrMut`, `PartsToSlice`, `PartsToMutSlice`) **stay** — they are still consumed by the unchanged Slice codegen-inline path.

Net: 24 IntrinsicId variants retire, 3 add (`@alloc`, `@realloc`, `@free`). Total registry shrinks by ~21 entries plus their `IntrinsicDef` records.

## Implementation Phases

Each phase ships behind the `vec_runtime_collapse` preview gate, ends with `make test` green, quotes its LOC delta in the commit message. Phases 1 and 2 are independent (could parallelize); 3–5 are strictly sequential.

- [x] **Phase 1: Memory intrinsics** *(~80 LOC added)*
  - Add `PreviewFeature::VecRuntimeCollapse` to `gruel-error/src/lib.rs`.
  - Add `IntrinsicId::Alloc` / `Realloc` / `Free` to `gruel-intrinsics/src/lib.rs` with `Expr` kind, `checked`-block requirement, preview gate to `vec_runtime_collapse`, runtime_fn populated.
  - Sema: type-check arity (2 for alloc, 4 for realloc, 3 for free), argument types (all `usize` except pointer args). Reject outside `checked` blocks.
  - Codegen: each translates to a direct LLVM extern call to the corresponding `__gruel_*` runtime symbol. Already-generated declarations in `gruel-codegen-llvm` (via the existing Vec lowering) — refactor those declarations into a shared "memory-intrinsics decl" helper.
  - Spec tests at `crates/gruel-spec/cases/intrinsics/memory.toml`: each intrinsic exercised in a `checked` block with a roundtrip alloc+write+read+free.
  - Verify `@ptr_cast(MutPtr(T), MutPtr(u8))` works in `checked` (or whatever the current cast intrinsic is — confirm and document).

- [ ] **Phase 2: Prelude Vec declaration** *(~150 LOC added in `prelude/vec.gruel`, no compiler changes yet)*
  - Create `prelude/vec.gruel` with the full `pub fn Vec(comptime T: type) -> type { ... }` declaration including all methods listed in §3.
  - The file is parsed by the existing prelude loader (no loader changes — `prelude/*.gruel` is already auto-discovered per ADR-0078).
  - **At this point the file exists but no code calls it.** The existing TypeKind::Vec dispatch still goes through the codegen-inline path. The prelude declaration is dead code until Phase 3.
  - Spec test: a no-op test that exercises a tiny program importing nothing — confirms the prelude file parses and instantiates without breaking other tests.
  - Note: this phase intentionally lands the Gruel source separately from the dispatch flip, so any parse / sema issue in the file is caught before mass test breakage.

- [ ] **Phase 3: Vec sema dispatch flip** *(~500 LOC out of `vec_methods.rs`; ~50 LOC added for the new dispatch helper)*
  - Rewrite `dispatch_vec_method_call` to look up methods on the prelude `Vec(T)` declaration rather than emitting `vec_*` intrinsics. Each call site produces a regular function-call AIR node to the instantiated `Vec(T)::method`.
  - `try_analyze_vec_index_read` / `try_analyze_vec_index_write` route to `Vec::index_read` / `Vec::index_write`.
  - `try_dispatch_vec_static_call` routes `Vec::new()` / `Vec::with_capacity(n)` to the prelude functions.
  - **Gate the flip behind `vec_runtime_collapse` preview**: when the gate is off, the old codegen-inline path runs (so Phase 3 is roll-backable); when on, the prelude path runs.
  - Run the full Vec spec test suite (`crates/gruel-spec/cases/vec/`) under the preview gate. Every test must pass.
  - This is the highest-risk phase — the entire Vec method dispatch surface rewires. Mitigations: gate behind preview, add side-by-side comparison tests (some tests run twice, once per dispatch path, for the duration of the phase).

- [ ] **Phase 4: Vec codegen retirement** *(~970 LOC out of `codegen.rs`)*
  - Delete the 16+ `translate_vec_*` functions listed in §5.
  - Delete the Vec match arms from the codegen dispatch table.
  - Delete `emit_vec_drop_loop` and the per-T `__drop_Vec_T` synthesis (Vec drop now goes through the standard Gruel-method drop dispatch).
  - Delete the 17 retired `IntrinsicId::Vec*` variants from `gruel-intrinsics`.
  - `@vec(...)` / `@vec_repeat(...)` desugar at sema time (added in this phase) to `with_capacity + push` chains.
  - `make test` green; this is the load-bearing verification that Phase 3's flip is bug-free.
  - Slice codegen / sema / IntrinsicId variants are **unchanged** — Slice migration is a separate ADR.

- [ ] **Phase 5: Stabilize** *(~50 LOC of polish)*
  - Remove `PreviewFeature::VecRuntimeCollapse`. The `@alloc` / `@realloc` / `@free` intrinsics' preview gate is removed (they remain `checked`-block-gated; whether to relax that further is the subject of Open Questions §3).
  - Spec section 7.3 gains an informative note pointing to `prelude/vec.gruel`. No normative paragraph changes.
  - ADR status → `implemented`.
  - ADR-0066 "Future Work" entry pointing at codegen-inline retirement gets marked resolved. ADR-0064's analogous entry stays open pending the second-class-struct mechanism (see Future Work).

## Consequences

### Positive

- **`gruel-codegen-llvm/src/codegen.rs` shrinks by ~970 LOC** (Vec codegen retirement). The remaining Vec codegen is the place-grammar / borrow-checker bridge plus the variadic literal lowering — small, focused.
- **Vec methods become user-readable.** Adding `Vec::last() -> Option(T)`, `Vec::find(p)`, etc. is an edit to `prelude/vec.gruel`. No coordinated registry / sema / codegen edit.
- **Allocation policy lives in Gruel.** The doubling-capacity grow heuristic, the minimum-first-cap = 4, the `cap == 0 ⇒ no allocation` invariant — all editable in Gruel source. Tuning becomes a one-file change.
- **`@alloc` / `@realloc` / `@free` become Gruel-callable** (in `checked` blocks). Future stdlib types — `HashMap`, `BTreeMap`, `Box(T)` — can use the same primitive substrate. This is independently useful.
- **`@vec(...)` and `@vec_repeat(v, n)` cost shifts off the codegen path.** Their lowering becomes "desugar to with_capacity + push," which inlines naturally. LLVM optimization quality may improve (the codegen-inline expansions of these were already optimization-friendly, but going through the standard call path opens additional inlining opportunities).
- **Drop synthesis simplifies.** The per-T `__drop_Vec_T` codegen synthesis path retires; standard drop dispatch picks up the prelude `Vec::drop` instantiation.
- **Establishes the playbook for Slice migration.** Once a second-class-struct mechanism lands, Slice can follow this ADR's pattern (prelude declaration + sema dispatch flip + codegen retirement) and reuse the `@alloc` / `@realloc` / `@free` infrastructure. The hard parts of the ADR-0078 stdlib pattern are validated for first-class types here.

### Negative

- **Largest single piece of compiler work in recent stdlib history.** ~1400 LOC of Rust retires across codegen, sema dispatch, and intrinsic registration; ~150 LOC of Gruel methods replace it. The sema dispatch flip (Phase 3) is high-risk because it rewires every Vec method call site.
- **Mitigated by phase staging.** Phases 1–2 ship dead-code prerequisites; Phase 3 flips behind a preview gate so the old path is always available for rollback; Phase 4 only deletes after Phase 3's flip has soaked in.
- **LLVM optimization quality could regress.** Today, `translate_vec_push` emits a hand-tuned LLVM sequence (conditional grow + write + len-inc). After migration, the same logic goes through Gruel source → standard call → standard inlining. LLVM's inliner is good, but a complex method body might not collapse as cleanly. Mitigation: run benchmark suite (ADR-0019, ADR-0043) at Phase 3 boundary; if a measurable regression surfaces, attribute the offending method as `@inline(always)` (if the language supports such an attribute — or add it as a follow-up).
- **`@alloc` / `@realloc` / `@free` are powerful primitives.** Exposing them as Gruel-callable broadens the language's surface for memory unsafety. Mitigated by `checked`-block gating; the existing ADR-0028 posture holds.
- **Element-wise iteration via raw pointer reads is more verbose than the codegen-inline path.** A Gruel `while i < self.len { let _ = ptr.add(i).read(); i = i + 1; }` loop is wordier than the equivalent LLVM IR. Acceptable: the verbosity lives in the prelude (one file, well-documented), not in user code.
- **Phase 3 has the largest test surface.** Every Vec spec test (~25 cases across `vec/types.toml`, `vec/methods.toml`, `vec/dispose.toml`) exercises method dispatch. A bug in the dispatch flip would surface broadly. Mitigated by side-by-side run mode for the duration of the gate.
- **Slice asymmetry persists.** After this ADR, Vec methods live in Gruel while Slice methods stay codegen-inline. Adding a method to both requires two different edit patterns. This is the price of leaving the second-class-struct mechanism for a follow-up; acceptable because (a) Slice's surface is small, (b) the asymmetry is well-flagged by the file split, and (c) the alternative (bundling the second-class mechanism into this ADR) risks a much larger landing.

### Neutral

- **Vec / Slice user-facing semantics are unchanged.** Construction, methods, indexing, slice borrows, drop, FFI handoff — observable behavior is identical. This is the load-bearing property of the migration.
- **The `TypeKind::Vec(_)` / `TypeKind::Slice(_)` markers stay.** Place-grammar and borrow-checker continue to recognize them. Generalizing to interface-driven dispatch is independent future work.
- **`__gruel_alloc` / `__gruel_realloc` / `__gruel_free` runtime symbols continue to exist.** No runtime-side reduction; the win is on the compiler side.

## Open Questions

1. **`prelude/vec.gruel` vs `prelude/collections/vec.gruel`?** The prelude is currently flat. Vec and (future) HashMap, BTreeMap, Slice (post-migration), etc. argue for a `collections/` subdirectory. The flat form is simpler for v1. Resolve by Phase 2; the directory shape is the same either way.

2. **`@alloc(size, align) -> MutPtr(u8)` byte-form vs `@alloc_n(T, n) -> MutPtr(T)` typed-form.** The byte form matches the runtime symbol shape exactly; the typed form is more ergonomic for the Vec body. Lean toward shipping the byte form first (load-bearing primitive) and adding typed wrappers as syntactic sugar in a follow-up. The Vec body's `let p_u8 = @alloc(...); let p = @ptr_cast(MutPtr(T), p_u8);` is mildly clunky but correct.

3. **Should `@alloc` / `@realloc` / `@free` graduate out of `checked`-block gating after Phase 5?** No, these should be checked.

4. **`@ptr_cast` interface.** The Vec body needs to convert `MutPtr(u8)` from `@alloc` into `MutPtr(T)`. What's the canonical way to express that today? If `@ptr_cast` exists, it's `@ptr_cast(MutPtr(T), p_u8)`. If not, this ADR adds one (~10 LOC of intrinsic + sema). Verify in Phase 1's first commit.

5. **Inlining quality for the Gruel-level `Vec::push`.** Today the codegen-inline `translate_vec_push` produces tight LLVM. After migration, the Gruel-level `push` body goes through standard inlining. Worth a benchmark at Phase 3 boundary; if there's a regression, decide whether to add an `@inline(always)`-style attribute (small new feature) or accept the regression.

6. **What about `@vec_from_array(arr)` (ADR-0066 future work)?** Out of scope; mention as future work.

7. **Does removing the `IntrinsicId::Vec*` variants break the doc-generator?** `make gen-intrinsic-docs` regenerates `docs/generated/intrinsics-reference.md` from the registry. Removing entries shrinks the doc; verify and update `make check-intrinsic-docs` baseline at Phase 4.

8. **Linear element support.** Per ADR-0067 (which silently superseded ADR-0066's "Linear elements" subsection — 0066's frontmatter doesn't carry the link), `Vec(T:Linear)` is *accepted*: the Vec value itself is linear (via `is_type_linear` recursion through `TypeKind::Vec(_)`), so it can never be implicitly dropped, and the user must call `Vec::dispose(self)` (panics if `len != 0`) after popping every element. The migration must preserve this. Specific things to verify in Phase 2:
   - **`is_type_linear` recursion still hits.** Today the recursion matches on `TypeKind::Vec(id)` and recurses into the element type. Since this ADR keeps `TypeKind::Vec(_)` as the value type (the prelude struct is the *method-body* substrate, not the value type), the recursion should keep working unchanged. Verify with a sema test using a `linear struct` element.
   - **`Vec::dispose(self)` body works for linear and non-linear `T`.** It panics if `len != 0`, then `@free`s the buffer without running the drop loop. The body has no per-element reads, so it doesn't trip the linearity checker on `T: Linear` instantiations.
   - **`Vec::drop(self)` body must not instantiate for `T: Linear`.** The drop body's `let _: T = self.ptr.add(i).read();` produces an owned `T` whose drop runs at scope exit — for `T: Linear` that is an implicit drop and a compile error. The intended behavior is that `Vec(T:Linear)::drop` is never called (the linear-discipline checker rejects any path that would implicitly drop the Vec), so the body never gets instantiated. Confirm Gruel's monomorphization is per-instantiation rather than pre-monomorphization (so the body's type-error stays dormant for linear T) — if it isn't, the body needs a comptime guard or two parallel `drop` functions.
   - **Which other methods reject `T: Linear`.** ADR-0066 noted that `clear` and indexed-write are inherently unsound for linear elements (both implicitly drop). Confirm the current implemented set of rejections (post-ADR-0067) and reproduce them in the prelude — likely as comptime asserts (`@assert(!@is_linear(T), "Vec::clear unavailable for linear T")`) at the top of those method bodies, or sema-level rejection keyed off the method name.

## Future Work

- **`Index` / `IndexMut` interfaces.** Today `v[i]` dispatches via TypeKind::Vec recognition; ADR-0078's operator overloading is comparisons-only. Adding `Index` / `IndexMut` (analogous to `Eq` / `Ord`) lets user-defined containers overload `[]`. This ADR doesn't need them — Vec's TypeKind recognition stays — but the generalization is the obvious next stop.
- **Typed allocation wrappers.** `@alloc_n(T, n) -> MutPtr(T)`, `@dealloc_n(T, p, n)` — friendlier for Vec-style bodies; pure syntactic sugar over the byte primitives.
- **Allocator parameterization** (`Vec(T, A)`). Same future work as ADR-0066.
- **Rich method surface for Vec** — `extend`, `insert`, `remove`, `swap_remove`, `truncate`, `drain`, `dedup`, `sort`, `iter`, `find`, `last`, `first`. Each is a one-edit add in `prelude/vec.gruel` after this ADR. (Slice's analogous additions wait on the migration above.)
- **Non-Copy `T: Eq` / `T: Clone` for the byte-search and clone methods.** Same shape as the deferred ADR-0066 Phase 11 work (per-element interface dispatch in inner loops).
- **HashMap / BTreeMap** in the prelude on top of the now-Gruel-callable `@alloc` / `@realloc` / `@free`. Direct beneficiaries of this ADR.
- **`@drop(value)` intrinsic.** Not needed by this ADR (scope-end drop suffices), but a more direct way to express "run T's drop on this value" could be useful for future stdlib types that don't fit the scope-end pattern.

## References

- ADR-0026: Module system (prelude resolution)
- ADR-0028: Unchecked code and raw pointers (`checked`-block gating for memory primitives)
- ADR-0029: Anonymous struct methods
- ADR-0050: Centralized intrinsics registry
- ADR-0061: Generic pointer types
- ADR-0063: Pointer operations as methods
- ADR-0064: Slices — defines the second-class Slice / MutSlice types this ADR leaves in place; their migration is queued as Future Work
- ADR-0065: Clone and Option (return shape for `Vec::pop`)
- ADR-0066: Vec(T) — the migrated substrate for Vec
- ADR-0070: Result(T, E)
- ADR-0078: Stdlib MVP (the comptime-function-returning-type pattern, the `prelude/` directory mechanism)
- ADR-0081: String runtime collapse (sibling ADR; ships first; establishes the playbook)
- Spec ch. 7.3 (Vec)

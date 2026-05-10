---
id: 0084
title: Markers for `Send` and `Sync`
status: proposal
tags: [types, ownership, concurrency, syntax, directives, intrinsics]
feature-flag: thread_safety
created: 2026-05-10
accepted:
implemented:
spec-sections: ["3.X", "4.X"]
superseded-by:
---

# ADR-0084: Markers for `Send` and `Sync`

## Status

Proposal

## Summary

Give every type a single thread-safety classification on the
trichotomy `Unsend < Send < Sync` and infer it structurally as the
**minimum** over a type's members. Primitives are intrinsically
`Sync`; raw pointers (`Ptr(T)` / `MutPtr(T)`) are intrinsically
`Unsend`. User types get three new override markers in the ADR-0083
registry: `@mark(unsend)` (always-safe downgrade), `@mark(checked_send)`
(claim Send despite a structural Unsend), and `@mark(checked_sync)`
(claim Sync despite a structural Send/Unsend). The `checked_` prefix
flags the upgrade markers as user-asserted ("compiler can't verify;
you take responsibility") — Rust's `unsafe impl Send` ergonomics in
directive form. To give the markers immediate teeth, this ADR also
adds a minimal `@spawn(fn, arg) → JoinHandle(R)` intrinsic that
requires its argument and return to be at least `Send`, and a linear
`JoinHandle(R)` built-in with a `join(self) → R` method.

Updating prelude containers (`Vec(T)`, `String`, `Option(T)`, etc.)
so that `Vec(i32)` infers as `Sync` rather than `Unsend` (the naive
result of containing a `MutPtr(T)`) is **out of scope** — those are
each one-PR follow-ups that consume this ADR's marker mechanism via
`comptime if` over `@thread_safety(T)` in the prelude (see Future
Work). Future `Mutex(T)` and `Arc(T)` follow the same pattern.

## Context

ADR-0083 stabilized the `@mark(...)` directive and the closed
`BUILTIN_MARKERS` registry, deliberately seeding it with only the
three posture markers (`copy` / `affine` / `linear`). Its Future
Work section flagged thread-safety as a natural extension. Two
forces make this the right time to land it:

1. **Concurrency is on the roadmap.** ADR-0011 explicitly notes
   "When Gruel adds threading, need mutex or thread-local arenas?"
   The compiler is single-threaded today, but every primitive that
   follows (Mutex, Arc, channels, statics) needs a way to talk
   about thread safety in the type system. Building Send/Sync now
   means future ADRs slot in instead of re-litigating the model.
2. **Affine ownership already encodes most of the work.** There is
   no `Rc`-equivalent, no interior mutability, no implicit
   aliasing. The only existing Gruel construct that breaks thread
   safety is the raw pointer — and that's confined to `checked`
   blocks. So the negative space starts small (one type), and the
   trichotomy can lean on what the type system already knows.

### Why a trichotomy instead of two orthogonal axes

Rust models thread safety as two axes:
- `T: Send` — `T` can be transferred across thread boundaries.
- `T: Sync` — `&T` can be shared across threads.

In practice almost every interesting type is either both Send + Sync
or neither. The `Sync + !Send` case (e.g. `MutexGuard`) is exotic and
arguably a design wart. For Gruel — which has affine ownership and
scope-bound references, so the "shared `&T` across threads" pattern
won't even surface for a long time — collapsing the two axes into a
single ladder simplifies everything: one field on each type, one min
operation for inference, one marker namespace, one `@spawn` check.

The trichotomy:

| Level    | Meaning                                                 |
|----------|---------------------------------------------------------|
| `Unsend` | Cannot cross thread boundaries.                         |
| `Send`   | Safe to move across threads (transferring ownership).   |
| `Sync`   | Safe to share across threads (subsumes `Send`).         |

Strict ordering: `Unsend < Send < Sync`. A `Sync` type is also
`Send`. The minimum-of-members rule is well-defined.

If the `Sync + !Send` case ever becomes important, the model can
grow to two axes later — this ADR closes no doors that aren't
already closed by Rust convention.

### Why the "checked_" naming convention for upgrade markers

ADR-0083's posture markers are all *assertions about structurally
verifiable facts*: `@mark(copy)` errors if any field is non-Copy;
`@mark(linear)` is contagious upward, never wrong. Send/Sync upgrade
markers are different — they encode user claims that the compiler
*cannot verify*. A struct holding a `MutPtr(T)` is structurally
`Unsend`, but the user might know the pointer is owned-exclusive and
the type is actually safe to send. That's an unsafe-style claim; the
compiler trusts the user.

Naming the markers `checked_send` and `checked_sync` (rather than
`send` and `sync`) signals this asymmetry in the markup itself. The
`checked_` prefix is a naming convention only — it does not require a
`checked { ... }` block on the declaration. This keeps `@mark(...)`
declaration-time-only, consistent with how every other marker behaves.

The opt-out marker is just `@mark(unsend)` (no prefix) because
downgrading is always safe — no claim is being made beyond
"restrict me further."

### Container updates are out of scope

`Vec(T)` is a prelude function (`prelude/vec.gruel`) whose internal
field is a `MutPtr(T)`. Under the structural rule, `Vec(i32)` would
infer as `Unsend` because of that pointer. Matching Rust (`Vec<T>:
Sync` iff `T: Sync`, via `unsafe impl<T: Sync> Sync for Vec<T>`) is
the right answer, but it requires Vec's prelude to override the
default — exactly what `@mark(checked_sync)` and friends are for.

Updating Vec is mechanical and isolated: wrap the existing struct
body in a `comptime if` over `@thread_safety(T)` and apply the
right marker on each branch. But it's a separate change from
introducing the marker mechanism — the ADR establishes the building
blocks; container updates consume them. Each prelude container
(`Vec`, `String`, `Option`, `Result`) gets its own follow-up PR.

This also means `Mutex(T)` and `Arc(T)` need no compiler-side
special knowledge for thread-safety inference. They become prelude
functions whose conditional safety claim lives in their own
`comptime if` over `@thread_safety(T)`, mirroring Rust's
`unsafe impl<T: Send> Sync for Mutex<T>` and `unsafe impl<T: Send +
Sync> Send + Sync for Arc<T>`. The detailed designs (clone
semantics, atomic refcount machinery, poison handling) are out of
scope for this ADR.

## Decision

### Trichotomy: `ThreadSafety` enum

Add to `gruel-builtins/src/lib.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ThreadSafety {
    Unsend,
    Send,
    Sync,
}
```

The variant order (`Unsend` first, `Sync` last) makes the derived
`Ord` impl give `Unsend < Send < Sync`, which is the ordering used
by the inference rule's `min` over members.

### Registry shape: `MarkerKind::ThreadSafety`

Extend `MarkerKind`:

```rust
pub enum MarkerKind {
    Posture(Posture),
    ThreadSafety(ThreadSafety),  // new
}
```

Append three rows to `BUILTIN_MARKERS`:

```rust
BuiltinMarker {
    name: "unsend",
    kind: MarkerKind::ThreadSafety(ThreadSafety::Unsend),
    applicable_to: ItemKinds::STRUCT_OR_ENUM,
},
BuiltinMarker {
    name: "checked_send",
    kind: MarkerKind::ThreadSafety(ThreadSafety::Send),
    applicable_to: ItemKinds::STRUCT_OR_ENUM,
},
BuiltinMarker {
    name: "checked_sync",
    kind: MarkerKind::ThreadSafety(ThreadSafety::Sync),
    applicable_to: ItemKinds::STRUCT_OR_ENUM,
},
```

`MarkerKind` is the same shape on the wire as today — the registry
just gains three rows. Since `Posture` and `ThreadSafety` live in
different `MarkerKind` variants, posture mutual exclusion (`copy` ⊥
`linear`) and thread-safety mutual exclusion (at most one of
`unsend` / `checked_send` / `checked_sync`) are checked
independently. A type may carry one of each:

```gruel
@mark(linear, checked_sync) struct LockedPool { ... }
```

`MarkOutcome` (`crates/gruel-air/src/sema/declarations.rs`) grows
one optional field:

```rust
pub thread_safety_override: Option<ThreadSafety>,
```

`process_mark_directives` writes it from `MarkerKind::ThreadSafety`
arguments. Multiple thread-safety markers on the same item are
rejected with a new `ConflictingThreadSafetyMarkers` diagnostic.

### Inference: structural minimum, with built-in facts

A new flag on every type-carrying struct (`StructDef`, `EnumDef`):

```rust
pub thread_safety: ThreadSafety,
```

Inference is implemented in `is_thread_safety_type(ty: Type) ->
ThreadSafety` on the type pool, mirroring the existing
`is_type_linear` shape:

```rust
pub fn is_thread_safety_type(&self, ty: Type) -> ThreadSafety {
    match ty.kind() {
        // Built-in negative facts:
        TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => ThreadSafety::Unsend,

        // Built-in positive facts (primitives are intrinsically Sync):
        TypeKind::I8 | TypeKind::I16 | TypeKind::I32 | TypeKind::I64
        | TypeKind::U8 | TypeKind::U16 | TypeKind::U32 | TypeKind::U64
        | TypeKind::Usize | TypeKind::Bool | TypeKind::Char
        | TypeKind::Unit | TypeKind::Never | TypeKind::F32 | TypeKind::F64
            => ThreadSafety::Sync,

        // Composite types: minimum over members.
        TypeKind::Array(array_id) => {
            let (elem, _len) = self.array_def(array_id);
            self.is_thread_safety_type(elem)
        }
        TypeKind::Tuple(tuple_id) => self.tuple_def(tuple_id).iter()
            .map(|&t| self.is_thread_safety_type(t))
            .min().unwrap_or(ThreadSafety::Sync),
        TypeKind::Ref(t) | TypeKind::MutRef(t) =>
            self.is_thread_safety_type(t),

        TypeKind::Struct(struct_id) => self.struct_def(struct_id).thread_safety,
        TypeKind::Enum(enum_id) => self.enum_def(enum_id).thread_safety,

        // ... (other kinds delegate to their members)
    }
}
```

Note: prelude containers like `Vec(T)` will infer as `Unsend` until
their prelude code is updated to apply the appropriate marker via
`comptime if` (see Future Work). That update is independent of this
ADR.

For named structs/enums, the `thread_safety` field on the def is
populated during the existing `validate_posture_consistency` pass
(renamed to `validate_consistency` to cover both axes):

1. **Inference pass.** Compute the structural minimum over members.
   Anonymous structs/enums use the same logic on the fly.
2. **Override pass.** If `MarkOutcome.thread_safety_override` is
   `Some(declared)`:
   - `declared = Unsend` → write `Unsend` (always safe).
   - `declared = Send` → write `Send`. Permitted regardless of
     inferred value (downgrade Sync → Send and upgrade Unsend → Send
     are both legitimate; the latter is the user-checked claim).
   - `declared = Sync` → write `Sync`. Permitted regardless of
     inferred value (the user-checked claim).

No diagnostic on legitimate downgrades — the user has stated their
intent, the compiler does what they asked.

A future ADR may add lints for redundant overrides
(`@mark(checked_send)` on a structurally `Send` type contributes
nothing). Not in this ADR.

### `@spawn(fn, arg) → JoinHandle(R)`

```gruel
fn worker(input: Job) -> Report { ... }

fn main() -> i32 {
    let handle: JoinHandle(Report) = @spawn(worker, Job { ... });
    let report: Report = handle.join();
    report.summary
}
```

Sema checks at the call site:

1. `worker` resolves to a top-level function (no methods, no
   anonymous functions; future work).
2. `worker` takes exactly one parameter.
3. `arg`'s type matches `worker`'s parameter type, in the existing
   bidirectional sense.
4. `worker`'s parameter type's `thread_safety` is `≥ Send`.
5. `worker`'s return type's `thread_safety` is `≥ Send`.
6. `worker`'s parameter type is not Linear and not a reference type
   (`Ref(T)` / `MutRef(T)`) — both for the obvious reason that the
   spawned thread outlives the caller's scope.

Failure modes get dedicated diagnostics: `SpawnArgNotSend`,
`SpawnReturnNotSend`, `SpawnArgIsRef`, `SpawnArgIsLinear`,
`SpawnFunctionWrongArity`, `SpawnFunctionNotFound`.

The single-argument shape is deliberate. With no closures (ADR-0055),
the call site cannot smuggle in extra captures, so multi-argument
spawn would need varargs or tuple-wrapping at the call site. Tuple
wrapping is the user's job here; the intrinsic surface stays small.

### `JoinHandle(R)` built-in

A new linear built-in container, lowered through the
`BuiltinTypeConstructor` registry from ADR-0061 (the same path
`Vec(T)` uses):

- **Linear posture.** Cannot be silently dropped; the user must
  `join` it. Mirrors `Vec(T:Linear)`'s must-consume discipline
  (ADR-0067).
- **`join(self) -> R`** consumes the handle and blocks until the
  spawned function returns, yielding the result.
- **Thread-safety: unconditionally `Send`.** The handle owns a
  thread-handle pointer; it does not store an `R` (the runtime
  writes `R` into a slot the handle owns, but `R` only flows back
  through `join`). `R: Send` is enforced at the `@spawn` site, so
  `JoinHandle(R)` is paired with a `Send` `R` by construction. The
  handle itself can be moved to another thread to join from there.
  Implementation: the synthetic struct definition carries
  `@mark(checked_send)` (the only built-in compiler-side use of an
  upgrade marker, justified by the handle being a compiler-internal
  type with no user-visible fields to override).
- **Drop backstop.** `__gruel_drop_JoinHandle` aborts the program
  with "JoinHandle dropped without join" — defensive net for runtime
  cases the linearity check misses.

### Comptime query intrinsic: `@thread_safety(T)`

One new intrinsic in `gruel-intrinsics`:

- `@thread_safety(T) -> ThreadSafety` — returns one of
  `ThreadSafety::Unsend`, `ThreadSafety::Send`, or
  `ThreadSafety::Sync`. Compile-time evaluated. Mirrors
  `@ownership(T)`.

Pattern:

```gruel
match @thread_safety(T) {
    ThreadSafety::Unsend => @compile_error("T must be sendable"),
    ThreadSafety::Send => /* OK */,
    ThreadSafety::Sync => /* OK, even better */,
}
```

The `ThreadSafety` enum is exposed in the prelude via the same
mechanism as `Ownership`: a synthetic enum injected by sema at
declaration-resolution time.

### Runtime support

Two new extern symbols in `gruel-runtime`:

- `__gruel_thread_spawn(thunk: *const u8, arg_buf: *mut u8,
  arg_size: usize, ret_size: usize) -> *mut ThreadHandle` — calls
  `pthread_create` (Unix) or `CreateThread` (Windows). The thunk is
  generated per `(arg type, return type)` pair and unwraps the
  Gruel-shaped call.
- `__gruel_thread_join(handle: *mut ThreadHandle, ret_out: *mut u8)
  -> ()` — joins the thread, copies the return value out, frees
  the handle.

The thunk is monomorphized per `@spawn` instantiation (same shape
`Vec(T)` methods are codegen'd today).

### What does not change

- Codegen for any code that doesn't use `@spawn` is unchanged.
- Posture inference (ADR-0083) is unchanged.
- The marker registry remains closed.
- Reference types (`Ref(T)` / `MutRef(T)`) remain scope-bound
  (ADR-0076). Sync has no surface beyond `JoinHandle` today.
- Allocator (libc malloc, ADR-0035) is already thread-safe.

## Implementation Phases

Each phase ships behind `--preview thread_safety`, ends green, and
quotes its LOC delta in the commit message.

### Phase 1: Trichotomy + structural inference (no enforcement)

- [ ] Add `ThreadSafety` enum to `gruel-builtins/src/lib.rs` with
      `#[derive(PartialOrd, Ord)]` so `Unsend < Send < Sync`.
- [ ] Extend `MarkerKind` with `ThreadSafety(ThreadSafety)`.
- [ ] Append `unsend`, `checked_send`, `checked_sync` rows to
      `BUILTIN_MARKERS`.
- [ ] Add `PreviewFeature::ThreadSafety` (`thread_safety`) to
      `gruel-util/src/error.rs`.
- [ ] Add `thread_safety: ThreadSafety` field to `StructDef` and
      `EnumDef` in `gruel-air`.
- [ ] Extend `MarkOutcome` with `thread_safety_override:
      Option<ThreadSafety>`, gated behind `thread_safety`.
- [ ] Implement `is_thread_safety_type(ty: Type) -> ThreadSafety`
      in `intern_pool.rs`, with built-in facts: primitives → Sync,
      pointers → Unsend, refs/arrays/tuples inherit structurally.
      Prelude container types (`Vec`, `String`, `Option`, etc.)
      are not special-cased here; they will pick up correct
      thread-safety in their own follow-up PRs by adding a
      `comptime if` over `@thread_safety(T)` to their prelude
      definitions.
- [ ] Implement structural inference for named structs/enums in
      `validate_consistency` (renamed from `validate_posture_consistency`):
      compute the min over members, then apply the override.
- [ ] Anonymous struct/enum literals get the same min-of-members
      treatment in `find_or_create_anon_struct` /
      `find_or_create_anon_enum`.
- [ ] Mutual exclusion: at most one thread-safety marker per item;
      conflict produces `ConflictingThreadSafetyMarkers`.
- [ ] Spec tests under `cases/items/thread-safety.toml`:
      `i32_is_sync`, `bool_is_sync`, `unit_is_sync`,
      `struct_of_primitives_is_sync`, `struct_with_ptr_is_unsend`,
      `tuple_of_primitives_is_sync`,
      `array_of_primitives_is_sync`,
      `unsend_marker_downgrades`,
      `checked_send_overrides_unsend`,
      `checked_sync_overrides_send`,
      `mutually_exclusive_thread_safety_markers`,
      `thread_safety_combined_with_posture_marker`,
      `mark_thread_safety_preview_gated`.
      Note: `vec_of_i32` / `string` thread-safety tests live with
      the respective container update PRs, not here.

### Phase 2: Built-in negative facts on raw pointers

- [ ] Verify Phase 1's `is_thread_safety_type` returns `Unsend` for
      `TypeKind::PtrConst(_)` and `TypeKind::PtrMut(_)`.
- [ ] Verify propagation through composite types:
      `Tuple(i32, MutPtr(u8))` is `Unsend`,
      `[MutPtr(i32); 4]` is `Unsend`, etc.
- [ ] Spec tests: `ptr_is_unsend`, `mutptr_is_unsend`,
      `array_of_ptr_propagates_unsend`,
      `tuple_with_ptr_propagates_unsend`,
      `struct_with_ptr_field_is_unsend`,
      `nested_struct_with_ptr_is_unsend`.

### Phase 3: Comptime query `@thread_safety`

- [ ] Add `IntrinsicId::ThreadSafety` to the `gruel-intrinsics`
      enum.
- [ ] Append the `IntrinsicDef` (kind: `Type`, category:
      `TypeReflection`, runtime_fn: `None`, preview:
      `Some(PreviewFeature::ThreadSafety)`).
- [ ] Inject a `ThreadSafety` enum into the prelude during sema's
      builtin-enum injection pass (alongside `Ownership`).
- [ ] Sema arm in `analyze_type_intrinsic` reads the type's
      `thread_safety`.
- [ ] Codegen lowers to a constant enum value at LLVM emission
      (compile-time constant; no runtime call).
- [ ] Run `make gen-intrinsic-docs` to regenerate
      `docs/generated/intrinsics-reference.md`.
- [ ] Spec tests: `thread_safety_returns_sync_for_i32`,
      `thread_safety_returns_unsend_for_ptr`,
      `thread_safety_returns_send_for_struct_with_checked_send`,
      `thread_safety_in_comptime_branch`.

### Phase 4: `JoinHandle(R)` built-in

- [ ] Define `JoinHandle` as a `BuiltinTypeConstructor` in
      `gruel-builtins`. Arity 1, lowers to a synthetic struct via
      the ADR-0020 mechanism.
- [ ] Posture: Linear (must-consume).
- [ ] Thread-safety: synthetic struct definition carries
      `@mark(checked_send)` (unconditionally Send; see Decision §
      `JoinHandle(R)` for rationale).
- [ ] Drop impl: `__gruel_drop_JoinHandle` aborts the program with
      a clear message — backstop only.
- [ ] `join(self) -> R` method registered in the BuiltinTypeDef
      method list, lowering to `__gruel_thread_join`.
- [ ] Spec tests: `join_handle_must_be_consumed`,
      `join_handle_join_returns_r`,
      `join_handle_is_send`.

### Phase 5: `@spawn` intrinsic + runtime support

- [ ] Add `IntrinsicId::Spawn` and the `IntrinsicDef` entry.
      Arguments: function reference + value; preview gated.
- [ ] Sema: resolve the function reference, check arity, check arg
      type matches parameter, check arg `≥ Send`, check return
      `≥ Send`, check arg is not Linear and not Ref/MutRef. New
      error kinds `SpawnArgNotSend`, `SpawnReturnNotSend`,
      `SpawnArgIsRef`, `SpawnArgIsLinear`,
      `SpawnFunctionWrongArity`, `SpawnFunctionNotFound`.
- [ ] Codegen: emit a per-instantiation thunk that adapts the C
      `void*(*fn)(void*)` calling convention to the Gruel function.
      Allocate the arg + return slot via the existing heap
      allocator, memcpy the arg, call `__gruel_thread_spawn`.
- [ ] Runtime: `__gruel_thread_spawn` and `__gruel_thread_join` in
      `gruel-runtime`, backed by `pthread_create` /
      `pthread_join` on Unix. Windows support deferred to Future
      Work.
- [ ] Panic policy: a panic in the spawned function aborts the
      whole process. Documented; future ADR can add `Result`-typed
      join.
- [ ] Spec tests: `spawn_basic_returns_value`,
      `spawn_arg_must_be_send`, `spawn_return_must_be_send`,
      `spawn_accepts_sync_arg`, `spawn_rejects_unsend_arg`,
      `spawn_rejects_ref_arg`, `spawn_rejects_linear_arg`,
      `spawn_rejects_wrong_arity`, `spawn_join_handle_is_linear`,
      `spawn_thunk_handles_zero_sized_return`.

### Phase 6: Spec text + corpus

- [ ] New spec section `docs/spec/src/03-types/15-thread-safety.md`
      describing the trichotomy, structural minimum inference, the
      `@mark(unsend)` / `@mark(checked_send)` /
      `@mark(checked_sync)` overrides, and the built-in facts
      (primitives → Sync, raw pointers → Unsend). Note in the
      section that prelude containers default to their structural
      inference until updated separately.
- [ ] New spec section `docs/spec/src/04-expressions/14-spawn.md`
      describing the `@spawn` intrinsic and `JoinHandle` semantics.
- [ ] Update `docs/spec/src/02-lexical-structure/05-builtins.md` to
      list `JoinHandle` and the `ThreadSafety` enum.
- [ ] Add a worked example to `examples/` demonstrating
      `@spawn(worker, job)` end-to-end.
- [ ] Regenerate `docs/generated/builtins-reference.md` and
      `docs/generated/intrinsics-reference.md`.

### Phase 7: Stabilize

- [ ] Remove `PreviewFeature::ThreadSafety` from
      `gruel-util/src/error.rs`. `--preview thread_safety` no
      longer recognized.
- [ ] Strip `preview = "thread_safety"` and `preview_should_pass =
      true` from every spec case still carrying them.
- [ ] `make test` passes on the final state, including the new
      thread-safety spec section in the traceability check.
- [ ] ADR status → `implemented`; frontmatter updated.

## Consequences

### Positive

- **Foundation for safe concurrency.** Every primitive that
  follows (`Mutex`, `Arc`, channels, statics) reads the existing
  `thread_safety` field instead of reinventing the taxonomy.
- **No new compiler-side type-pool special cases for containers.**
  The thread-safety inference engine knows about primitives and
  raw pointers; everything else uses the structural rule.
  Container-specific overrides (`Vec(i32)` is `Sync`, `Mutex(T)`
  is conditionally `Sync`, etc.) live in prelude code via
  `comptime if` + the new markers, mirroring Rust's
  `unsafe impl<...>` machinery rather than growing compiler magic.
- **Single trichotomy is simpler than two axes.** One field per
  type, one min operation, one marker namespace, one `@spawn`
  check. The exotic `Sync + !Send` case is closed off (matching
  Rust convention's de facto invariant).
- **`@spawn` gives the markers immediate teeth.** Without an
  enforcement site the markers would bit-rot; the minimal spawn
  shape produces real "this argument is not Send" errors.
- **Closed marker registry stays clean.** Three rows added; the
  closed taxonomy holds. No new directive form, no new keyword.
- **The `checked_` naming convention is self-documenting.**
  Reading `@mark(checked_send)` in a struct head immediately
  signals "the user is making an unverifiable claim here" — code
  reviewers can scan for it the same way they scan for `unsafe`
  in Rust.

### Negative

- **Sync surface is small today.** The trichotomy is fully
  meaningful only once shared references / `Mutex` / `Arc` /
  statics exist. Until then the `Sync` distinction matters only
  for forward-looking comptime checks.
- **`@spawn` is initially limited to primitives and primitive-only
  structs.** Until each prelude container's follow-up PR lands,
  `Vec(i32)`, `String`, `Option(i32)`, etc. infer as `Unsend`
  (their internal `MutPtr` poisons the structural minimum). Realistic
  usage — `@spawn(worker, vec_of_jobs)` — waits on those follow-ups.
  This is acceptable for a foundation ADR but means the initial
  end-to-end story is small.
- **Adds a runtime dependency on libpthread.** Builds that don't
  use `@spawn` are unaffected (the symbol is only emitted on
  demand), but the test matrix expands to cover threaded
  execution.
- **Per-instantiation thunks for `@spawn`.** Each `@spawn(fn,
  arg)` call produces a generated trampoline keyed on `(arg type,
  return type)`. Code-size cost in programs that spawn many
  distinct types; typical usage spawns a small number of worker
  shapes.
- **`JoinHandle` is the first non-`Vec` linear built-in.** Some
  of the linear-container plumbing from ADR-0067 was Vec-shaped;
  making it work for `JoinHandle` may surface assumptions.
  Bounded: the ADR-0067 design is generic in principle.
- **No closures means no captures.** Workers can't reference
  outer state without explicit argument-passing. Acceptable for
  v1 (matches ADR-0055's stance) but limits ergonomics. A future
  closure ADR would enable thread::spawn-style captures.

### Neutral

- Codegen for non-spawn code is unchanged.
- Posture semantics, Copy ⊥ Drop, `@ownership(T)`,
  `@implements(T, I)`: all unchanged.
- The `@mark` directive grammar is unchanged. Three new argument
  names, same shape.

## Open Questions

1. **`@spawn` argument shape: single, variadic, or tuple-required?**
   The proposal is single-argument with the user wrapping in a
   tuple for multi-arg cases. Variadic would be more ergonomic but
   complicates the intrinsic signature and the thunk codegen. Could
   revisit if tuple-wrapping is awkward in practice.

2. **`@spawn` panic policy.** Currently a panic in the spawned
   function aborts the whole process. The Rust convention is
   `Result<R, Box<dyn Any>>` from `join`. Aborting is simpler and
   doesn't pull in any error-handling design questions; switching
   to Result is non-breaking (a future ADR can change the join
   return type behind a preview gate).

3. **Lints for redundant overrides.** `@mark(checked_send)` on a
   structurally `Send` type contributes nothing. Should it be a
   warning? Defer to a future lint ADR.

4. **Should `ThreadSafety::Send + !Sync` ever be representable for
   user types in a future revision?** The trichotomy closes the
   door on `Sync + !Send`. The reverse case (`Send + !Sync`) is
   covered: a struct holding a `MutPtr` with `@mark(checked_send)`
   produces exactly that. So the trichotomy doesn't lose
   expressiveness in the direction users actually need.

5. **Negative facts on raw pointers: configurable in the future?**
   Today `Ptr(T)` is unconditionally `Unsend`. A future
   `NonAliasingPtr(T)` or similar could be `Send` or `Sync`. This
   ADR keeps the built-in fact as-is; future negative-pointer
   types get added to the constructor registry with their own
   thread-safety rules.

## Future Work

This ADR is intentionally narrow. Out of scope:

- **Prelude container thread-safety updates.** `Vec(T)`, `String`,
  `Option(T)`, `Result(T, E)`, and any other prelude container
  with an internal `MutPtr` infers as `Unsend` after this ADR
  lands. Each gets a one-PR follow-up that wraps its body in a
  `comptime if` over `@thread_safety(T)`:
  ```gruel
  pub fn Vec(comptime T: type) -> type {
      comptime if (@thread_safety(T) == ThreadSafety::Sync) {
          @mark(checked_sync)
          struct { ptr: MutPtr(T), len: usize, cap: usize, ... }
      } else if (@thread_safety(T) == ThreadSafety::Send) {
          @mark(checked_send)
          struct { ... }
      } else {
          struct { ... }   // structurally Unsend, no override
      }
  }
  ```
  No compiler change needed; the markers and `@thread_safety`
  intrinsic from this ADR are sufficient. The pattern is already
  proven to work — `comptime if` selecting between
  differently-marked struct definitions in a
  `fn(comptime T: type) -> type` body produces the right
  posture/marker behavior.
- **`Mutex(T)`, `Arc(T)`, channels.** Each will be its own ADR.
  They live in the prelude as `pub fn Mutex(comptime T: type) ->
  type` / `pub fn Arc(comptime T: type) -> type` and use the same
  `comptime if` + marker pattern as the container updates above —
  no compiler-side type-pool arms needed. Mutex's rule: `Sync` if
  `T >= Send`, else `Unsend`. Arc's rule: `Sync` if `T == Sync`,
  else `Unsend`.
- **Stored references (`Ref(T)` / `MutRef(T)` in struct fields).**
  Out of scope per ADR-0062. When that lands, `MutRef(T)` may need
  to drop to `Send` (not `Sync`) — the inference rule for the
  reference types would slot into Phase 1's propagation.
- **Detached threads.** No way to opt out of joining today; every
  spawn returns a linear `JoinHandle`. A future
  `@spawn_detached(F, arg)` could exist for fire-and-forget cases.
- **Thread-local storage.** No `thread_local!` equivalent.
- **`Result`-typed join.** Currently a panic in the worker aborts
  the process. Future ADR could change `join(self) -> R` to
  `join(self) -> Result(R, ThreadPanic)`.
- **Closures with captured environments.** Would let `@spawn`
  take a true closure instead of a function reference + arg.
  Depends on a real closure ADR.
- **Windows thread support.** `@spawn` runtime backs onto pthread
  in v1; Windows port is a follow-up.
- **Two-axis (Send + Sync) refinement.** If the trichotomy ever
  proves too coarse — specifically if the `Sync + !Send` case
  becomes important — split `ThreadSafety` into two flags. The
  marker namespace already has `unsend` / `checked_send` /
  `checked_sync` cleanly separated; the migration would be
  mechanical.

## References

- [ADR-0005: Preview Features](0005-preview-features.md)
- [ADR-0008: Affine Types and the MVS](0008-affine-types-mvs.md)
- [ADR-0011: Runtime Heap](0011-runtime-heap.md) — flags threading
  as a future concern.
- [ADR-0028: Unsafe and Raw Pointers](0028-unsafe-and-raw-pointers.md)
- [ADR-0050: Intrinsics Crate](0050-intrinsics-crate.md)
- [ADR-0055: Anonymous Functions](0055-anonymous-functions.md) —
  no runtime captures.
- [ADR-0061: Generic Pointer Types](0061-generic-pointer-types.md) —
  `BuiltinTypeConstructor` registry reused for `JoinHandle`.
- [ADR-0062: Reference Types Replacing Borrow Modes](0062-reference-types.md)
- [ADR-0066: Vec Type](0066-vec-type.md) — first prelude container
  that will need a follow-up update to apply the new markers.
- [ADR-0067: Linear Containers](0067-linear-containers.md) —
  must-consume discipline reused for `JoinHandle`.
- [ADR-0070: Result Type](0070-result-type.md) — relevant if
  `join` ever becomes Result-typed.
- [ADR-0076: Pervasive Self and Sole-Form References](0076-pervasive-self-and-sole-form-references.md)
- [ADR-0080: `copy` Keyword for Copy Types](0080-copy-keyword.md)
- [ADR-0083: `@mark(...)` Directive](0083-mark-directive.md) —
  host registry this ADR extends.
- [Rust: `Send` and `Sync`](https://doc.rust-lang.org/nomicon/send-and-sync.html)

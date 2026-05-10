+++
title = "Thread Safety"
weight = 15
+++

# Thread Safety

This section describes how Gruel classifies types for thread-boundary
crossings. The classification is a single trichotomy ordered as
`Unsend < Send < Sync`.

## Trichotomy

{{ rule(id="3.15:1", cat="normative") }}

Every type **MUST** carry exactly one of three thread-safety
classifications:

- **`Unsend`** — values of this type **MUST NOT** cross a thread
  boundary.
- **`Send`** — values of this type **MAY** be moved across a thread
  boundary, transferring ownership.
- **`Sync`** — values of this type **MAY** be shared across thread
  boundaries by reference. A `Sync` type is also `Send`.

{{ rule(id="3.15:2", cat="normative") }}

The classifications form a strict ordering: `Unsend < Send < Sync`.

## Built-in Facts

{{ rule(id="3.15:3", cat="normative") }}

The following types are intrinsically `Sync`:

- All integer types (`i8`–`i64`, `u8`–`u64`, `isize`, `usize`).
- All floating-point types (`f16`, `f32`, `f64`).
- The boolean type (`bool`).
- The character type (`char`).
- The unit type (`()`).
- The never type (`!`).

{{ rule(id="3.15:4", cat="normative") }}

The raw pointer types `Ptr(T)` and `MutPtr(T)` are intrinsically
`Unsend` regardless of `T`. Containing types **MAY** override this
classification with `@mark(checked_send)` or `@mark(checked_sync)` on
the host struct/enum head.

## Structural Inference

{{ rule(id="3.15:5", cat="normative") }}

For composite types not covered by a built-in fact (named structs,
named enums, anonymous structs/enums, arrays, tuples, references,
slices, vectors), the thread-safety classification is the **minimum**
over the classifications of all members (fields, variant payloads,
elements, referents).

{{ rule(id="3.15:6", cat="normative") }}

An empty composite (zero fields, zero variants, zero-length array)
**MAY** be treated as `Sync` — `Sync` is the identity element for the
structural-minimum operation.

{{ rule(id="3.15:7", cat="example") }}

```gruel
struct Point { x: i32, y: i32 }   // Sync (every field is Sync)
struct Handle { ptr: MutPtr(u8) } // Unsend (raw pointer poisons the min)
```

## Markers

{{ rule(id="3.15:8", cat="normative") }}

Three marker names recognized inside the `@mark(...)` directive
(ADR-0083) override a type's structurally-inferred thread-safety:

- `unsend` — downgrades the type to `Unsend`. Always permitted; the
  marker only restricts.
- `checked_send` — declares the type `Send`. The compiler does **NOT**
  verify the claim; the user takes responsibility for the assertion
  (analogous to Rust's `unsafe impl Send`). The `checked_` prefix
  flags the marker as user-asserted.
- `checked_sync` — declares the type `Sync`. Same caveat as
  `checked_send`.

{{ rule(id="3.15:9", cat="legality-rule") }}

At most one thread-safety marker **MAY** be applied to any single
type. Applying more than one of `unsend`, `checked_send`,
`checked_sync` to the same declaration is a compile-time error.

{{ rule(id="3.15:10", cat="example") }}

```gruel
@mark(checked_send) struct Job { id: i32, queue: MutPtr(u8) }
// Structurally Unsend (because of the MutPtr field), but the user
// asserts the type is Send-safe in practice.
```

{{ rule(id="3.15:11", cat="normative") }}

Posture markers (`copy` / `affine` / `linear`) and thread-safety
markers occupy independent namespaces; one of each **MAY** appear on
the same type.

{{ rule(id="3.15:12", cat="example") }}

```gruel
@mark(linear, checked_sync) struct LockedPool { /* ... */ }
```

{{ rule(id="3.15:13", cat="informative") }}

Prelude container types (`Vec(T)`, `String`, `Option(T)`, `Result(T,
E)`, …) currently infer their thread-safety structurally — typically
`Unsend` due to internal raw pointer fields. Container-specific
overrides (e.g. `Vec(T)` being `Sync` when `T` is `Sync`) are added
in follow-up changes that wrap the container body in `comptime if`
over `@thread_safety(T)`.

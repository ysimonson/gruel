---
id: 0070
title: Canonical Result(T, E)
status: proposal
tags: [types, generics, prelude, error-handling]
feature-flag: result_type
created: 2026-05-01
accepted:
implemented:
spec-sections: ["3.10"]
superseded-by:
---

# ADR-0070: Canonical Result(T, E)

## Status

Proposal

## Summary

Introduce `Result(T, E) = enum { Ok(T), Err(E) }` as a canonical, prelude-injected generic enum, paralleling `Option(T)` from ADR-0065. The infrastructure is already in place — comptime-generic enum monomorphization (ADR-0025), enum data variants (ADR-0037), exhaustive pattern matching (ADR-0052), Clone propagation (ADR-0065), and linearity propagation through enums (ADR-0067) — so this ADR is primarily a *registration and method-surface* layer, not a new compiler-machinery layer. v1 ships a minimal method set (`is_ok`, `is_err`, `ok`, `err`, `unwrap`, `unwrap_err`, `unwrap_or`, plus `expect` / `expect_err`); higher-order combinators (`map`, `map_err`, `and_then`, `or_else`) wait until the comptime-generic anon-function shape stabilizes (the same gating reason `Option::map` was deferred in ADR-0065). The `?` operator and `From`-style error conversion are explicitly out of scope — they're a separate ADR with their own design questions. Linear element types follow the same protocol ADR-0067 established for `Option(T:Linear)`: linearity propagates, `unwrap` is rejected, users must `match` exhaustively.

## Context

### Why now

Three forcing functions pile on at once:

1. **ADR-0072 needs `Result` for its conversion APIs.** `String::from_utf8(v: Vec(u8))` should return the original `Vec(u8)` on failure so the caller can recover or report; today it would have to return `Option(String)` and discard `v`. ADR-0072 explicitly flags this as a v1 limitation pending `Result`. Without `Result`, every fallible-with-recovery API in the language has the same hole.
2. **ADR-0071 (`char::from_u32`) flagged the same hole.** A `Result(char, u32)` is a strict improvement over `Option(char)` for diagnostics — preserving the offending input is what makes a useful error.
3. **The infrastructure is ready.** ADR-0065 demonstrated the canonical-prelude-enum pattern (`Option`); ADR-0067 extended it to handle linearity propagation. `Result` is the obvious second canonical sum type. Building it now means the `?` operator follow-up (which is a substantial addition) has a stable canonical type to desugar against.

Without `Result`, every caller that wants "succeed-with-payload, fail-with-context" defines a one-off enum. This fragments the ecosystem the same way ad-hoc `Option`-equivalents did before ADR-0065.

### What's already there

- **`Option(T)`** (ADR-0065) — the precedent. Canonical name, registered via the prelude source string injected at `FileId::PRELUDE`. Methods (`is_some`, `unwrap`, etc.) live in the prelude alongside the type. Pattern-matches via standard ADR-0037 enum machinery.
- **Comptime-generic enums** (ADR-0025 / ADR-0039) — `fn Result(comptime T: type, comptime E: type) -> type { enum { Ok(T), Err(E) } }` is already expressible.
- **Linearity propagation through enums** (ADR-0067) — `Option(T:Linear)` reports as linear; same recursion handles `Result(T:Linear, E)`, `Result(T, E:Linear)`, and both linear.
- **Niche optimization through layout** (ADR-0069) — `Result(T, E)` automatically benefits when `T` or `E` carries niches (e.g., `Result(char, char)` will be 4 bytes once char's niches are registered).

### What this ADR does *not* attempt

- **The `?` operator.** Desugaring `expr?` into a `match expr { Ok(x) => x, Err(e) => return Err(e) }` is the easy part; making it interoperable with different error types requires a `From`-style conversion mechanism (Rust's `?` calls `From::from(e)`). Without that, `?` only works when error types match exactly — useful but limiting. Building `?` in the same ADR as `Result` itself bundles two design questions (sum type, error conversion) that are better separated. Follow-up: ADR-007X for `?` + error conversion.
- **`From`-style error conversion / interfaces.** Out of scope. Also a follow-up.
- **`map`, `map_err`, `and_then`, `or_else`.** ADR-0065's Phase 5 deferred `Option::map` because the comptime-generic anon-function path requires both `T` and the return-type parameter to express `Option(U)` from `f: T -> U`. The same gating applies here. Once that path lands (single shared follow-up), `map`/`map_err`/etc. arrive simultaneously on both `Option` and `Result`.
- **`Result(T:Linear, E)` and `Result(T, E:Linear)` ergonomics.** Linearity propagates correctly (the recursion from ADR-0067 already covers it), but methods like `unwrap` and `unwrap_err` are rejected for linear payloads (panic path leaks). v1 leaves users to `match` exhaustively — same posture as ADR-0067 took for `Option(T:Linear)`. A `dispose` mechanism for `Result` is not meaningful (both arms always have a payload), so there's nothing to design here.
- **`try_*` collection methods.** APIs like `Vec(T)::try_push` would naturally return `Result`, but adding them to existing collections is out of scope here. Once `Result` is canonical, those APIs can be added incrementally without needing another ADR for the type.

### Where Gruel lands

- **Rust:** `Result<T, E>` with rich method surface, `?` operator, `From`-conversion. Gruel's destination is similar; this ADR ships the type and a smaller method surface, deferring `?` and conversion.
- **Swift:** `Result<Success, Failure: Error>` constrains `Failure` to an `Error` protocol. Gruel doesn't constrain `E` — any type works. (No protocol exists today; if one's added later, it can be opt-in for `?` interop, not for `Result` itself.)
- **OCaml / Haskell:** `result` / `Either`. Same shape. Gruel matches the ML/Rust side.
- **Go:** multiple return values, no `Result`. We deliberately don't follow Go's path — exhaustive pattern matching is more useful than the discipline of "always check `err != nil`."

## Decision

### 1. The type

A canonical generic enum, defined in the prelude:

```gruel
fn Result(comptime T: type, comptime E: type) -> type {
    enum {
        Ok(T),
        Err(E),
    }
}
```

Registered alongside `Option` via the same prelude-injection mechanism (parsed first under `FileId::PRELUDE`). Users write `Result(i32, String)` anywhere a type is expected, no import needed.

### 2. Layout

Standard ADR-0037 enum-with-data: tag + payload union sized to the larger of `T` and `E`, plus padding for alignment.

When `T` or `E` carries niches (per ADR-0069), the niche-filling pass elides the discriminant byte. Examples:
- `Result(bool, bool)` — both arms are 1-byte-with-niche; result is 1 byte.
- `Result(char, ())` — char's niches absorb the `Err` variant; 4 bytes, no tag.
- `Result(i32, ())` — i32 has no niches; standard layout (8 bytes: tag + i32 + padding).
- `Result(char, char)` — char's niches accommodate the discriminant; 4 bytes.

No special-case logic; it falls out of ADR-0069's existing infrastructure.

### 3. Method surface (v1)

| Method | Receiver | Signature | Notes |
|---|---|---|---|
| `is_ok` | `&self` | `(&self) -> bool` | true iff `Ok` |
| `is_err` | `&self` | `(&self) -> bool` | true iff `Err` |
| `ok` | `self` | `(self) -> Option(T)` | `Ok(t) -> Some(t)`; `Err(_) -> None` (drops E) |
| `err` | `self` | `(self) -> Option(E)` | `Ok(_) -> None` (drops T); `Err(e) -> Some(e)` |
| `unwrap` | `self` | `(self) -> T` | panic if `Err`; move `t` out otherwise. Requires `T: !Linear` and `E: !Linear`. |
| `unwrap_err` | `self` | `(self) -> E` | panic if `Ok`. Same linearity restrictions. |
| `unwrap_or` | `self` | `(self, default: T) -> T` | `default` consumed only on `Err`. |
| `expect` | `self` | `(self, msg: String) -> T` | panic with `msg` if `Err`. |
| `expect_err` | `self` | `(self, msg: String) -> E` | panic with `msg` if `Ok`. |

Panic messages for `unwrap` / `unwrap_err`: fixed strings (`"called `unwrap` on an `Err` value"` / `"called `unwrap_err` on an `Ok` value"`) routed through the existing panic infrastructure.

`map`, `map_err`, `and_then`, `or_else` ship in the same follow-up that lifts `Option::map`, since they share the same comptime-generic anon-function constraint.

### 4. Linearity

Same protocol as `Option` (ADR-0067):

- **Linearity propagates.** `Result(T, E)` is linear iff `T: Linear` ∨ `E: Linear` ∨ the generic-recursion machinery flags it. The existing recursion in `is_type_linear` handles enums with payloads.
- **`unwrap` / `unwrap_err` / `unwrap_or` / `expect` / `expect_err` are rejected when either payload is linear.** The panic path mid-`unwrap` would leak the *other* variant's linear payload (the one we panic instead of returning). Users must `match` exhaustively.
- **`is_ok` / `is_err` work for any `T, E`** (they take `&self`, no consumption).
- **`ok` / `err` work** as long as the *dropped* arm is non-linear. Dropping the `Err` payload in `r.ok()` requires `E: !Linear`; symmetric for `err()`. Sema enforces this with a clear error.
- **No `dispose`.** Unlike `Option(T)::dispose` (which is meaningful when the variant is `None`, i.e., no live linear payload), `Result(T, E)` always has a live payload. There is no "empty" state to dispose. The right answer is `match`.

This means linear-payload `Result` types are ergonomically thin in v1 (only `is_ok`/`is_err` and conditional `ok`/`err`). That's deliberate — a richer story for linear sum types is a separate follow-up.

### 5. `Clone` conformance

`Result(T, E)` is `Clone` iff `T: Clone` and `E: Clone`. Synthesized at registration time via the same `@derive(Clone)`-equivalent path used for `Option(T)` (ADR-0065).

Since v1 enums are uniformly `Copy` (ADR-0065 §3.8:2 simplification), `Result(T, E)` is automatically `Copy` (and therefore `Clone`) when both `T` and `E` are `Copy`. The hand-written enum-clone synthesis kicks in once that simplification is refined.

### 6. Pattern matching

Falls out of ADR-0037 / ADR-0049 / ADR-0052 with no additions:

```gruel
let r: Result(i32, String) = Result::Ok(42);
match r {
    Result::Ok(n) => use(n),
    Result::Err(e) => report(e),
}
```

Exhaustiveness checking already covers the two-variant case. No new matching machinery.

Open detail: should `Ok` and `Err` be importable as bare names (`Ok(42)` instead of `Result(i32, String)::Ok(42)`)? `Option`'s `Some` and `None` are bare — see ADR-0065 §"Migration." Mirror that: `Ok` and `Err` are bare-importable from the prelude. Same well-known-name registry.

### 7. Compiler integration

- **`gruel-builtins` / prelude:** add the `Result(T, E)` definition and v1 method bodies to the prelude source string injected under `FileId::PRELUDE`. Add `Ok` and `Err` to the bare-importable name registry alongside `Some` / `None`.
- **`gruel-air`:** no new infrastructure. Sema resolves `Result` through the prelude exactly as it resolves `Option`. Linearity propagation already handles enum payloads. The `unwrap` / `ok` / `err` linearity gates use the existing `is_type_linear` query.
- **`gruel-codegen-llvm`:** no changes. Falls out of ADR-0037 enum codegen and ADR-0069 niche-filling.
- **Spec:** new section `3.10 The Result(T, E) type` (or wherever the prelude appendix sits), parallel to `Option(T)`'s section. Documents validity, layout (refers to ADR-0037 + ADR-0069), method surface, linearity rules.

## Implementation Phases

- [ ] **Phase 1: Preview gate + prelude scaffolding**
  - Add `PreviewFeature::ResultType` to `gruel-error`.
  - Append `Result(T, E)` definition and a stub method body (`is_ok` only) to the prelude source string.
  - Register `Ok` and `Err` as bare-importable names.
  - Confirm name resolution and basic match work.
- [ ] **Phase 2: Core method surface**
  - Implement `is_ok`, `is_err`, `unwrap`, `unwrap_err`, `unwrap_or` in the prelude.
  - `unwrap` / `unwrap_err` linearity gates (mirrors `Option::unwrap`).
  - Spec tests for each method, including panic behavior.
- [ ] **Phase 3: Conversions to Option**
  - `ok(self) -> Option(T)` and `err(self) -> Option(E)`.
  - Linearity gates on the dropped arm.
  - Spec tests cover round-trips and linear-rejection cases.
- [ ] **Phase 4: `expect` / `expect_err`**
  - Implement using the existing panic-with-message infrastructure.
  - Spec tests.
- [ ] **Phase 5: Linearity propagation tests**
  - Verify `Result(MustUse, i32)`, `Result(i32, MustUse)`, `Result(MustUse, MustUse)` all report as linear.
  - Verify the rejection diagnostics for `unwrap` / `ok` / `err` on linear arms.
  - No new code expected — existing recursion should cover it; phase exists to confirm and document.
- [ ] **Phase 6: Clone conformance**
  - Verify `Result(i32, i32)` is `Copy` (hence `Clone`) under the v1 enum-Copy simplification.
  - Add a deferred-synthesis note for when ADR-0065's simplification is refined.
- [ ] **Phase 7: Niche optimization tests**
  - `Result(bool, bool)` is 1 byte; `Result(char, ())` is 4 bytes (after ADR-0071 lands).
  - No new code; verify ADR-0069's niche-filling consumes Result's discriminant correctly.
- [ ] **Phase 8: Spec**
  - Write spec section 3.10 (or place under existing prelude appendix).
  - Cross-link from `Option(T)`'s section.
- [ ] **Phase 9: Stabilize**
  - Remove preview gate.
  - Update consumer ADRs (ADR-0072's `from_utf8` return type; ADR-0071's `char::from_u32`).

## Consequences

### Positive

- Canonical fallible-with-context return type. Eliminates ad-hoc per-call-site enums.
- `String::from_utf8` (ADR-0072) can return the original `Vec(u8)` on failure — the open question in that ADR resolves cleanly.
- `char::from_u32` (ADR-0071) gains the option of returning the offending `u32` for diagnostics.
- Niche-optimized layouts come for free via ADR-0069.
- Linearity story is consistent with `Option`'s — no new design surface.
- Foundation for the `?` operator follow-up.

### Negative

- v1 method surface is small. `map`, `map_err`, `and_then`, `or_else` matter for ergonomic chaining and are deferred. Mitigated by the explicit pattern-match path always being available.
- No `?` operator yet means error propagation is verbose (`match` at every layer). This is the v1 cost; the follow-up resolves it.
- Linear-payload `Result` is even thinner than non-linear (only `is_ok`/`is_err` and conditional `ok`/`err`). Acceptable for v1 — the use case is rare.
- Adding `Ok` / `Err` to the bare-importable name space commits two short, common identifiers globally. Anyone wanting `let Ok = ...` as a variable name has a problem. Mitigation: the canonical-name registry already commits `Some`, `None`, `String`, etc.; `Ok`/`Err` are in keeping.

## Open Questions

- **Bare-import `Ok` / `Err` vs qualified `Result::Ok` / `Result::Err`?** Match what was done for `Option` in ADR-0065.
- **Should `unwrap_or` take `default: T` by value (consume) or by closure (`|| -> T`)?** ADR-0065's `Option::unwrap_or` consumes. Match that for symmetry. The lazy form (`unwrap_or_else`) waits for the same anon-function follow-up that gates `map`.
- **Should `expect` take `msg: String` (owned) or `msg: &str` (borrowed)?** Borrowed slices aren't a stable type yet; pass owned `String` for v1. Migrate to `&str` when borrowed slices land.

## Future Work

- **`?` operator + `From`-style error conversion** — separate ADR. The big ergonomics win.
- **`map`, `map_err`, `and_then`, `or_else`** — ship together with `Option::map` once the comptime-generic anon-function path is stable. Single follow-up ADR.
- **`try_*` collection methods** (`try_push`, `try_reserve`) — incremental additions to existing collections.
- **`Result(T, E)` for linear types** — richer methods (e.g., a `match`-like "consume both arms" helper) if the ergonomic gap proves real.

## References

- ADR-0025: Comptime generics.
- ADR-0037: Enum data variants.
- ADR-0049 / ADR-0052: Pattern matching.
- ADR-0065: Clone interface and canonical Option(T).
- ADR-0067: Linear types in containers.
- ADR-0069: Layout abstraction and niche-filling.
- ADR-0072: String / Vec(u8) relationship.
- ADR-0071: char type.
- Rust's `Result<T, E>` and `?` operator documentation.

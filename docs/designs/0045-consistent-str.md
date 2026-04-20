---
id: 0045
title: Consistent String Interface and Comptime String Materialization
status: proposal
tags: [comptime, types, strings]
feature-flag: consistent_str
created: 2026-04-20
accepted:
implemented:
spec-sections: [4.14]
superseded-by:
---

# ADR-0045: Consistent String Interface and Comptime String Materialization

## Status

Proposal

## Summary

Standardize the method set between `comptime_str` and runtime `String` so that query and producing methods are available in both contexts, and add auto-materialization so that `comptime_str` values can escape comptime blocks as runtime `String` values.

## Context

ADR-0042 introduced `comptime_str` as a comptime-only string type for metaprogramming. It works well for its intended purpose — receiving field names from `@typeInfo`, building error messages for `@compileError`, and string manipulation during compile-time evaluation.

However, two gaps have emerged:

### Gap 1: Inconsistent method sets

`comptime_str` and `String` share many of the same conceptual operations but have different APIs:

| Method | `comptime_str` | Runtime `String` |
|--------|---------------|------------------|
| `.len()` | `-> i32` | `-> u64` |
| `.is_empty()` | `-> bool` | `-> bool` |
| `.contains(s)` | `-> bool` | *missing* |
| `.starts_with(s)` | `-> bool` | *missing* |
| `.ends_with(s)` | `-> bool` | *missing* |
| `.concat(s)` | `-> comptime_str` | *missing* |
| `.clone()` | *missing* | `-> String` |
| `.capacity()` | *missing* | `-> u64` |
| `.push_str(s)` | *missing* | mutates in place |
| `.push(byte)` | *missing* | mutates in place |
| `.clear()` | *missing* | mutates in place |
| `.reserve(n)` | *missing* | mutates in place |
| `==`, `!=` | yes | yes |
| `<`, `<=`, `>`, `>=` | yes | *missing* |

Users who know `comptime_str` has `.contains()` may expect `String` to have it too, and vice versa.

### Gap 2: No materialization path

Currently, if a `comptime { }` block evaluates to a `comptime_str`, the compiler emits: "comptime_str values cannot exist at runtime". This prevents useful patterns like:

```gruel
fn get_type_name(comptime T: type) -> String {
    comptime { @typeName(T) }   // ERROR: comptime_str values cannot exist at runtime
}
```

Materializing a comptime string to a runtime `String` is straightforward — it's the same operation as a string literal (`AirInstData::StringConst`). Comptime integers already materialize automatically; strings should too.

## Decision

### 1. Unified method set

Add missing methods to each side so that query and producing methods work in both `comptime_str` and `String` contexts. Mutation methods remain runtime-only.

#### Query methods (immutable, both contexts)

| Method | `comptime_str` signature | `String` signature | Notes |
|--------|--------------------------|--------------------|-------|
| `.len()` | `(self) -> i32` | `(self) -> u64` | Already exists in both |
| `.is_empty()` | `(self) -> bool` | `(self) -> bool` | Already exists in both |
| `.contains(needle)` | `(self, needle: comptime_str) -> bool` | `(self, needle: String) -> bool` | **Add to String** |
| `.starts_with(prefix)` | `(self, prefix: comptime_str) -> bool` | `(self, prefix: String) -> bool` | **Add to String** |
| `.ends_with(suffix)` | `(self, suffix: comptime_str) -> bool` | `(self, suffix: String) -> bool` | **Add to String** |

#### Producing methods (return new value, both contexts)

| Method | `comptime_str` signature | `String` signature | Notes |
|--------|--------------------------|--------------------|-------|
| `.concat(other)` | `(self, other: comptime_str) -> comptime_str` | `(self, other: String) -> String` | **Add to String** |
| `.clone()` | `(self) -> comptime_str` | `(self) -> String` | **Add to comptime_str** |

#### Mutation methods (runtime `String` only)

| Method | Signature | Notes |
|--------|-----------|-------|
| `.push_str(other)` | `(inout self, other: String)` | Existing |
| `.push(byte)` | `(inout self, byte: u8)` | Existing |
| `.clear()` | `(inout self)` | Existing |
| `.reserve(n)` | `(inout self, n: u64)` | Existing |
| `.capacity()` | `(self) -> u64` | Existing |

#### Operators

| Operator | `comptime_str` | `String` | Notes |
|----------|---------------|----------|-------|
| `==`, `!=` | yes | yes | Already exists in both |
| `<`, `<=`, `>`, `>=` | yes | **Add to String** | Lexicographic byte ordering |

#### Design notes

- **`.len()` returns different types** depending on context: `i32` in comptime (consistent with comptime integer semantics where all values are `i64` surfaced as `i32`), `u64` at runtime (matching the existing ABI).

- **`.concat()` exists alongside `.push_str()`** because they serve different needs. `.concat()` is a pure operation that returns a new string — it works naturally in comptime where values are immutable. `.push_str()` mutates the receiver in place via `inout self` — this requires runtime mutation semantics.

- **`.clone()` is added to `comptime_str`** for API consistency. In comptime, it simply copies the string to a new heap slot.

- **Mutation methods are runtime-only.** `comptime_str` values are immutable on the comptime heap. Calling a mutation method on a `comptime_str` produces a compile error: "cannot call .push_str() on a compile-time string; use .concat() to produce a new string."

- **`.capacity()` is runtime-only.** Comptime strings have no allocation — capacity is meaningless.

- **Ordering operators** use lexicographic byte comparison on runtime `String`, matching the existing `comptime_str` implementation.

### 2. Auto-materialization

When a `comptime { }` block evaluates to a `ConstValue::ComptimeStr`, the compiler materializes it as a runtime `String` constant instead of producing an error.

```gruel
fn get_type_name(comptime T: type) -> String {
    comptime { @typeName(T) }   // Now works — materializes as runtime String
}
```

Materialization reuses the existing `AirInstData::StringConst` mechanism — the same path used for string literals. The comptime string's content is extracted from the comptime heap, added to the function's local string table via `add_local_string()`, and emitted as a `StringConst` instruction with the builtin `String` type.

```rust
// In the InstData::Comptime handler, replace the ComptimeStr error:
ConstValue::ComptimeStr(str_idx) => {
    let content = self.resolve_comptime_str(str_idx, span)?.to_string();
    let ty = self.builtin_string_type();
    let local_string_id = ctx.add_local_string(content);
    let air_ref = air.add_inst(AirInst {
        data: AirInstData::StringConst(local_string_id),
        ty,
        span,
    });
    Ok(AnalysisResult::new(air_ref, ty))
}
```

This mirrors how comptime integers are materialized as `IntConst` and comptime bools as `BoolConst`.

### 3. Before and after

```gruel
// Before: error
fn describe(comptime T: type) -> String {
    comptime { @typeName(T) }   // ERROR: comptime_str values cannot exist at runtime
}

// After: works
fn describe(comptime T: type) -> String {
    comptime { @typeName(T) }   // "Point" materialized as runtime String
}

// Before: no contains/starts_with on runtime String
fn check(s: String) -> bool {
    s.contains("hello")         // ERROR: no method 'contains' on String
}

// After: works
fn check(s: String) -> bool {
    s.contains("hello")         // calls String__contains at runtime
}
```

### 4. Spec changes

Update spec section 4.14 (Comptime Strings):
- Update rule 4.14:56 to allow `comptime_str` values to escape to runtime via materialization as `String`
- Add a new rule documenting auto-materialization behavior
- Update rule 4.14:58 to include `.clone()` in the `comptime_str` method list
- Add new spec rules for runtime `String` methods: `contains`, `starts_with`, `ends_with`, `concat`
- Add new spec rules for runtime `String` ordering operators
- Add spec rules for comptime mutation error messages

## Implementation Phases

- [ ] **Phase 1: Runtime String query methods** — Add `contains`, `starts_with`, `ends_with` as runtime methods. In `gruel-builtins` (`STRING_TYPE`), add three new `BuiltinMethod` entries with `receiver_mode: ByRef` and a `SelfType` parameter (the needle/prefix/suffix). In `gruel-runtime/src/string.rs`, implement `String__contains`, `String__starts_with`, `String__ends_with` — each takes `(ptr, len, cap, other_ptr, other_len, other_cap)` and returns `u8` (0 or 1). Add spec tests.

- [ ] **Phase 2: Runtime String concat and clone** — Add `concat` as a runtime method in `gruel-builtins` (`STRING_TYPE`) with `receiver_mode: ByRef`, one `SelfType` parameter, and `return_ty: SelfType`. In `gruel-runtime`, implement `String__concat` — allocates a new string with the concatenation of both inputs, returns via `StringResult`. Add spec tests.

- [ ] **Phase 3: Runtime String ordering operators** — Add `Lt`, `Le`, `Gt`, `Ge` operators to `STRING_TYPE.operators` in `gruel-builtins`. Implement `__gruel_str_lt`, `__gruel_str_le`, `__gruel_str_gt`, `__gruel_str_ge` in `gruel-runtime` (or implement as a single `__gruel_str_cmp` returning `i32` and derive each operator from it). Add spec tests.

- [ ] **Phase 4: Comptime `clone` method** — Add `clone` to `evaluate_comptime_str_method` in `analysis.rs`. Implementation: copy the string content to a new comptime heap slot and return `ConstValue::ComptimeStr(new_idx)`. Add spec tests.

- [ ] **Phase 5: Auto-materialization** — In the `InstData::Comptime` handler in `analysis.rs`, replace the `ConstValue::ComptimeStr` error with code that materializes the comptime string as a runtime `String` via `AirInstData::StringConst` (extract content from comptime heap, call `add_local_string`, emit `StringConst` with `builtin_string_type()`). Add tests for `comptime { "hello" }`, `comptime { @typeName(T) }` escaping to runtime.

- [ ] **Phase 6: Comptime mutation errors** — In `evaluate_comptime_str_method`, add arms for `push_str`, `push`, `clear`, `reserve`, and `capacity` that return a clear error: "cannot call .<method>() on a compile-time string; use .concat() to produce a new string" (or "capacity is not available for compile-time strings"). This prevents confusing "unknown method" errors when users try runtime-only methods on `comptime_str`. Add UI tests.

- [ ] **Phase 7: Spec & tests** — Update spec section 4.14: modify rule 4.14:56 (materialization), update rule 4.14:58 (add `.clone()`), add rules for runtime `String` methods and operators, add rules for comptime mutation errors. Update existing spec tests. Verify traceability.

## Consequences

### Positive

- Consistent API — query and producing methods work on both string types, reducing surprise when switching between comptime and runtime contexts
- Comptime string values can escape to runtime, enabling patterns like `comptime { @typeName(T) }` as a runtime `String`
- Runtime `String` gains useful methods (`contains`, `starts_with`, `ends_with`, `concat`) and ordering operators that it was missing
- Clear error messages guide users who try runtime-only mutations on `comptime_str` toward using `.concat()` instead
- Materialization reuses existing `StringConst` infrastructure — minimal new codegen complexity

### Negative

- `comptime_str` and `String` remain distinct types — users still need to understand they're different (one is comptime-only, one is runtime). This ADR intentionally does not unify them
- `.len()` returns `i32` in comptime and `u64` at runtime, which is a subtle difference. This is inherent to how comptime integers work (all `i64` surfaced as `i32`) and is not new
- `.concat()` and `.push_str()` coexist with overlapping purpose (concatenation), but they serve different semantics (pure vs mutating)
- Adding 4+ new runtime functions increases the `gruel-runtime` surface area

### Neutral

- `comptime_str` retains its name and identity — this is a targeted unification of capabilities, not a type merge
- No parser changes needed
- No new type system concepts — `comptime_str` and `String` remain separate types with a shared method vocabulary

## Open Questions

None.

## References

- ADR-0042: Comptime Metaprogramming (introduced `comptime_str`)
- ADR-0020: Builtin Types as Structs (String type architecture)
- ADR-0014: Mutable Strings (String mutation methods)

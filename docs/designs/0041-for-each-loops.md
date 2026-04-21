---
id: 0041
title: For-Each Loops
status: implemented
tags: [syntax, semantics]
feature-flag: for_loops
created: 2026-04-19
accepted: 2026-04-19
implemented: 2026-04-21
spec-sections: []
superseded-by:
---

# ADR-0041: For-Each Loops

## Status

Implemented

## Summary

Add `for`-`in` loop syntax for iterating over arrays and integer ranges. Ranges are first-class values produced by a `@range` intrinsic and typed as `Range(T)`, a builtin comptime type constructor parameterized by integer type. When range bounds are comptime-known, for-loops compile to optimal C-style counter loops with no runtime overhead.

## Context

Currently, Gruel has `while` and `loop` for iteration. Iterating over an array or a counted range requires manually managing an index variable:

```gruel
let arr: [i32; 4] = [10, 20, 30, 40];
let mut sum = 0;
let mut i = 0;
while i < 4 {
    sum = sum + arr[i];
    i = i + 1;
}
```

This is verbose and error-prone (off-by-one on the bound, forgetting to increment, etc.). A `for`-`in` construct is the most-requested idiom for iteration in any imperative language and eliminates an entire class of bugs.

Two forms are needed:
1. **For-each over arrays**: `for x in arr { ... }` — iterate over each element.
2. **For over ranges**: `for i in @range(10) { ... }` — equivalent to C's `for (int i = 0; i < 10; i++)`.

Gruel's comptime system (ADR-0025) already provides monomorphization, comptime type constructors, and anonymous struct types — all the building blocks for first-class ranges without needing a separate generics mechanism.

## Decision

### Syntax

```ebnf
for_expr  = "for" [ "mut" ] IDENT "in" expression "{" block "}" ;
```

- `for x in expr { body }` — iterate, binding each element to `x`.
- `for mut x in expr { body }` — bind as mutable (allows modifying the loop variable inside the body, but does **not** write back to the source).
- The iterable expression must be either an array or a `Range(T)` value.

### The `Range(T)` type

`Range` is a builtin comptime type constructor, parameterized by an integer type. Conceptually it is equivalent to:

```gruel
fn Range(comptime T: type) -> type {
    struct {
        start: T,
        end: T,
        stride: T,
        inclusive: bool,

        fn inclusive(self) -> Self {
            Self {
                start: self.start,
                end: self.end,
                stride: self.stride,
                inclusive: true,
            }
        }
    }
}
```

The compiler knows about `Range` like it knows about `String` — it is injected as a builtin, not defined in user code. `Range(T)` can appear in type annotations, function parameters, and variable bindings:

```gruel
fn sum_range(r: Range(i32)) -> i32 {
    let mut total = 0;
    for i in r {
        total = total + i;
    }
    total
}
```

Generic functions over ranges use the existing comptime parameter mechanism:

```gruel
fn count(comptime T: type, r: Range(T)) -> i32 {
    let mut n = 0;
    for _ in r {
        n = n + 1;
    }
    n
}
```

### The `@range` intrinsic

`@range` constructs `Range(T)` values. It accepts 1, 2, or 3 integer arguments:

| Form | Meaning | C equivalent |
|------|---------|-------------|
| `@range(end)` | `0` to `end`, exclusive, stride 1 | `for (i = 0; i < end; i++)` |
| `@range(start, end)` | `start` to `end`, exclusive, stride 1 | `for (i = start; i < end; i++)` |
| `@range(start, end, stride)` | `start` to `end`, exclusive, step by `stride` | `for (i = start; i < end; i += stride)` |

All forms set `inclusive = false` by default. Chain `.inclusive()` to make the range include `end`:

| Form | Meaning | C equivalent |
|------|---------|-------------|
| `@range(end).inclusive()` | `0` to `end`, inclusive, stride 1 | `for (i = 0; i <= end; i++)` |
| `@range(start, end).inclusive()` | `start` to `end`, inclusive | `for (i = start; i <= end; i++)` |
| `@range(start, end, stride).inclusive()` | `start` to `end`, inclusive, step by `stride` | `for (i = start; i <= end; i += stride)` |

Rules:
- All arguments must be the same integer type `T`. The result has type `Range(T)`.
- `@range` produces exclusive ranges by default. Call `.inclusive()` to include `end`.
- `stride` must be non-zero. A literal `0` stride is a compile-time error; a non-literal `0` stride panics at runtime.
- Negative strides are supported: `@range(10, 0, -1)` iterates `10, 9, 8, ..., 1`. With a negative stride, the loop condition is `i > end` (or `i >= end` when inclusive) instead of `i < end`.
- If `start >= end` (positive stride, exclusive) or `start > end` (positive stride, inclusive) or the reverse for negative strides, the body never executes.

```gruel
// Sum 0..9
let mut sum = 0;
for i in @range(10) {
    sum = sum + i;
}
// sum == 45

// Sum 5..9
let mut sum = 0;
for i in @range(5, 10) {
    sum = sum + i;
}
// sum == 35

// Even numbers: 0, 2, 4, 6, 8
let mut sum = 0;
for i in @range(0, 10, 2) {
    sum = sum + i;
}
// sum == 20

// Countdown: 5, 4, 3, 2, 1
let mut count = 0;
for i in @range(5, 0, -1) {
    count = count + 1;
}
// count == 5

// Inclusive range: 0 through 10
let mut sum = 0;
for i in @range(10).inclusive() {
    sum = sum + i;
}
// sum == 55

// Inclusive + stride: all u8 values (avoids overflow that @range(0u8, 256u8) would cause)
let mut count: i32 = 0;
for b in @range(0u8, 255u8).inclusive() {
    count = count + 1;
}
// count == 256
```

#### Comptime evaluation of `@range`

When all arguments to `@range` are comptime-known, the resulting `Range(T)` value is comptime-evaluatable. This means:

```gruel
// The range is fully known at compile time — the compiler emits
// the same code as: let mut i = 0; while i < 10 { ...; i += 1; }
for i in @range(10) {
    // ...
}

// Ranges can be constructed in comptime blocks
const R: Range(i32) = comptime { @range(0, 100, 2) };
```

When range bounds are runtime values, the range fields are runtime values and the loop is a standard while loop — still efficient, just not compile-time-folded:

```gruel
fn iterate_up_to(n: i32) {
    for i in @range(n) {  // n is runtime, so start/end are runtime
        @dbg(i);
    }
}
```

#### Storing and passing ranges

Because `Range(T)` is a first-class type, ranges can be stored in variables and passed to functions:

```gruel
let r = @range(10);         // r: Range(i32)
let r2 = @range(0u64, 100u64);  // r2: Range(u64)

// Pass to a function
fn process(r: Range(i32)) {
    for i in r { @dbg(i); }
}
process(@range(5, 15));
```

`Range(T)` is a copy type (its fields are integers and a bool, all of which are copy types). Passing a range to a function copies it.

### For-each over arrays

```gruel
let arr: [i32; 3] = [10, 20, 30];
let mut sum = 0;
for x in arr {
    sum = sum + x;
}
// sum == 60
```

**Copy types**: Each element is copied into the loop variable. The array remains valid after the loop.

**Move types**: Each element is moved out of the array. The array is consumed by the loop. This is the only way to iterate and consume an array of move types without indexing each element individually.

```gruel
struct Data { value: i32 }

fn main() -> i32 {
    let arr: [Data; 2] = [Data { value: 1 }, Data { value: 2 }];
    let mut sum = 0;
    for d in arr {
        // d owns the Data; arr is being consumed
        sum = sum + d.value;
    }
    // arr is no longer valid here
    sum
}
```

**Partial consumption**: `break` inside a for-each loop over move types is a compile error, because it would leave some elements unconsumed and the array in a partially-moved state. (For copy types, `break` is allowed.)

### Type and expression rules

- A `for` expression has type `()`, like `while`.
- The loop variable is immutable by default. Use `for mut x in ...` to make it mutable.
- The iterable expression is evaluated exactly once, before the loop begins.

### Desugaring

For-each loops are desugared during AstGen (RIR generation). No new IR instructions are needed — the existing `Loop` (while), `Break`, `Continue`, variable, and indexing instructions are reused.

#### Range desugaring

`for i in range_expr` where `range_expr` has type `Range(T)` desugars to:

For positive stride, exclusive (`inclusive = false`):
```
let __range = range_expr;      // evaluate once
let mut __iter = __range.start;
let __end = __range.end;
let __stride = __range.stride;
while __iter < __end {
    let i = __iter;            // immutable binding visible in body
    body
    __iter = __iter + __stride;
}
```

For positive stride, inclusive (`inclusive = true`), the condition becomes `__iter <= __end`.

For negative stride, the conditions are `__iter > __end` (exclusive) or `__iter >= __end` (inclusive).

When stride sign or inclusive flag is not comptime-known, the desugaring emits runtime branches to select the correct comparison. When they are comptime-known (the common case — e.g., `@range(10)` or `@range(0, 10).inclusive()`), the compiler emits a single loop with the correct comparison directly.

Note: even though the source says `for i`, the desugared counter `__iter` is mutable (it must be incremented). If the programmer did not write `for mut i`, the inner `let i = __iter` is immutable — reads of `i` inside the body are allowed but assignments to `i` are not.

When `for mut i` is used, the inner `let` becomes `let mut i = __iter` and modifications to `i` do not affect iteration (the counter is `__iter`).

#### Array desugaring

`for x in arr { body }` desugars to:

```
let __arr = arr;               // evaluate once (move or copy)
let mut __i: u64 = 0;
let __len: u64 = @len(__arr);  // compile-time-known array length
while __i < __len {
    let x = __arr[__i];        // copy or move element
    body
    __i = __i + 1;
}
```

For move-type elements, the compiler tracks that each element is moved exactly once (the loop runs for exactly `N` iterations over `[T; N]`). `break` is forbidden to ensure all elements are consumed.

### New tokens

| Token | Representation |
|-------|---------------|
| `for` | keyword |
| `in` | keyword |

`for` and `in` are not currently reserved — they must be added to the keyword list and the reserved word spec section. No new operator tokens are needed; `@range` reuses the existing intrinsic syntax and `Range(T)` reuses the existing comptime type constructor syntax.

## Implementation Phases

- [x] **Phase 1: Lexer, parser, and Range type** — Add `for`, `in` keywords to the lexer. Add `ForExpr` AST node to the parser. Register `Range` as a builtin comptime type constructor in `gruel-builtins` (struct with `start`, `end`, `stride` fields and `inclusive` bool field; `.inclusive()` method). Add preview feature gate `for_loops`. Update keyword spec section.
- [x] **Phase 2: `@range` intrinsic and range for-loops** — Implement `@range(...)` intrinsic that constructs `Range(T)` values (1-, 2-, and 3-argument forms, `inclusive = false` by default). Desugar `for x in range_val { body }` to while-loop patterns in AstGen, selecting `<`/`<=`/`>`/`>=` based on stride sign and `inclusive` flag. Validate integer types, stride constraints. Support `break`/`continue`. Add comptime evaluation support for `@range`. Add spec tests for both exclusive and inclusive ranges.
- [x] **Phase 3: Array for-loops (copy types)** — Desugar `for x in arr { body }` to indexed while-loop in AstGen. Support `break`/`continue`. Array remains valid after the loop. Add spec tests.
- [x] **Phase 4: Array for-loops (move types)** — Handle move semantics: array is consumed by the loop. Forbid `break` when iterating over move-type arrays. Add spec tests.
- [x] **Phase 5: Spec, warnings, and polish** — Write the spec section for for-each loops (likely §4.8 extension or new §4.9). Add `@range` and `Range(T)` to the intrinsics/types spec sections. Add unused loop variable warnings. Update grammar appendix. Run full test suite.

## Consequences

### Positive
- Eliminates the most common source of off-by-one errors in loops
- Familiar syntax for programmers coming from Rust, Python, Swift, Go, etc.
- `Range(T)` is a first-class type — ranges can be stored, passed, and returned
- Comptime evaluation of range bounds means optimal code generation for literal ranges
- Leverages existing comptime infrastructure (monomorphization, type constructors) — no new type system concepts
- Range loops compile to the same efficient code as hand-written while loops (no runtime overhead)
- Desugaring to existing IR means no changes needed in CFG or codegen
- Move-type array consumption via for-each is ergonomic and safe
- No new operator tokens — `@range` reuses existing intrinsic syntax

### Negative
- `break` restriction on move-type arrays may be surprising
- `Range(T)` adds a new builtin type, increasing the surface area of `gruel-builtins`
- `@range(10)` is slightly more verbose than Rust's `0..10`, though the 3-argument stride form is cleaner than method chaining

## Resolved Questions

- Should `for` loops over empty arrays of move types be allowed? (The array is trivially consumed since there are no elements.) Yes.
- Should we allow `for x in [1, 2, 3] { ... }` with inline array literals, or require a named binding? Desugaring handles this naturally (the literal is evaluated once into a temporary), so yes.
- Should `Range(T)` fields (`start`, `end`, `stride`) be publicly accessible? Yes — they're just struct fields.
- Should stride sign determination use comptime evaluation when possible, falling back to runtime branching? Yes — comptime-known strides emit a single loop direction, runtime strides emit a branch.

## Future Work

- **Iterators / `Iterable` trait**: A trait-based protocol for user-defined iteration, enabling `for x in my_collection { ... }`.
- **Enumerate**: `for (i, x) in arr.enumerate() { ... }`.
- **Range methods**: `.contains()`, `.len()`, `.is_empty()` on Range values.

## References

- [ADR-0005: Preview Features](0005-preview-features.md)
- [ADR-0025: Compile-Time Execution (comptime)](0025-comptime.md)
- [Spec §4.8: Loop Expressions](../../spec/src/04-expressions/08-loop-expressions.md)
- [Spec §4.13: Intrinsic Expressions](../../spec/src/04-expressions/13-intrinsics.md)
- [Spec §4.14: Compile-Time Expressions](../../spec/src/04-expressions/14-comptime.md)
- [Spec §3.5: Array Types](../../spec/src/03-types/05-array-types.md)
- [Spec §3.8: Move Semantics](../../spec/src/03-types/08-move-semantics.md)

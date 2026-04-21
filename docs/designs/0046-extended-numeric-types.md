---
id: 0046
title: Extended Numeric Types (i128/u128, isize/usize, f16/f32/f64/f128, comptime_int)
status: proposal
tags: [types, syntax, codegen]
feature-flag: extended_numeric_types
created: 2026-04-20
accepted:
implemented:
spec-sections: ["3.1", "3.5"]
superseded-by:
---

# ADR-0046: Extended Numeric Types (i128/u128, isize/usize, f16/f32/f64/f128, comptime_int)

## Status

Proposal

## Summary

Add the remaining Rust-equivalent numeric primitive types to Gruel: 128-bit integers (`i128`, `u128`), pointer-sized integers (`isize`, `usize`), IEEE 754 floating-point types (`f16`, `f32`, `f64`, `f128`), and a compile-time integer type (`comptime_int`). This brings Gruel beyond Rust's numeric parity (which lacks `f16`/`f128`) and matches Zig's float coverage, enabling real-world systems programming workloads.

## Context

Gruel currently supports `i8`/`i16`/`i32`/`i64` and `u8`/`u16`/`u32`/`u64`. This covers common cases but leaves important gaps:

1. **`isize`/`usize`**: Required for safe array/slice indexing and memory-sized quantities. Today Gruel has no pointer-sized integer, making array indexing inherently un-portable across 32-bit and 64-bit targets.

2. **`i128`/`u128`**: Needed for cryptographic operations, UUIDs, IPv6 addresses, and high-precision intermediate calculations. Rust and Zig both support these.

3. **`f16`/`f32`/`f64`/`f128`**: Floating-point math is essential for scientific computing, graphics, game development, audio processing, and general-purpose programming. Without floats, Gruel cannot serve as a general-purpose language. `f16` is increasingly important for ML inference and GPU interop. `f128` enables high-precision scientific computation and matches Zig's float coverage.

### Design constraints

- The `Type` encoding uses a u32 with tag bits 0–13 for primitives and 100+ for composites. Adding 9 new primitive tags (14–22) fits cleanly within the existing scheme.
- LLVM natively supports all of these types (`i128`, `half`, `float`, `double`, `fp128`, plus target-specific `isize`/`usize` mapping to `i32` or `i64`).
- Floating-point introduces a new literal syntax (`3.14`, `1e10`) and new semantic considerations (NaN, infinities, comparison semantics).

## Decision

### New types

| Gruel type | LLVM type | Size | Notes |
|------------|-----------|------|-------|
| `i128` | `i128` | 16 bytes | Signed 128-bit integer |
| `u128` | `i128` | 16 bytes | Unsigned 128-bit integer |
| `isize` | `i32` or `i64` | target-dependent | Pointer-sized signed integer |
| `usize` | `i32` or `i64` | target-dependent | Pointer-sized unsigned integer |
| `f16` | `half` | 2 bytes | IEEE 754 binary16 (half-precision) |
| `f32` | `float` | 4 bytes | IEEE 754 binary32 (single-precision) |
| `f64` | `double` | 8 bytes | IEEE 754 binary64 (double-precision) |
| `f128` | `fp128` | 16 bytes | IEEE 754 binary128 (quad-precision) |
| `comptime_int` | N/A (compile-time only) | 8 bytes (i64) | Compile-time integer, analogous to `comptime_str` |

### `comptime_int`

`comptime_int` is a compile-time-only integer type, analogous to the existing `comptime_str`. It is the type of integer expressions evaluated at compile time within `comptime` blocks. Internally represented as an `i64`.

Key properties:
- **Not a runtime type**: Cannot appear in function signatures, struct fields, or variable types. It exists only during comptime evaluation.
- **Coerces to any integer type**: When a `comptime_int` value flows into a runtime context, it is implicitly narrowed to the expected integer type (with a compile-time range check).
- **Follows the `comptime_str` pattern**: Added to `TypeKind` and `Type` as a primitive tag. Cannot be interned in the type pool. Is Copy.
- **Use case**: Enables comptime functions that compute indices, sizes, or other integer values that feed into runtime code — e.g., `comptime { array_len * 2 }` returning a `comptime_int` that coerces to `usize`.

### Literal syntax

**Integer literals** remain unchanged — `42` defaults to `i32`, type-inferred as today.

**Floating-point literals** use the syntax:

```gruel
let a = 3.14;       // f64 (default)
let b: f32 = 3.14;  // f32 via type inference
let c = 1e10;       // f64 (scientific notation)
let d = 1.5e-3;     // f64 (scientific notation with decimal)
let e = 0.0;        // f64
```

Floating-point literals default to `f64` (matching Rust). A literal with a `.` or `e`/`E` is always floating-point. There is no suffix syntax (`3.14f32`) — use type annotations instead.

**Disambiguating integers from floats**: `42` is always an integer. `42.0` is always a float. `42.method()` is a method call on integer `42` — the parser must handle this (see Implementation Phases).

### Type encoding

Extend the primitive tag range in `Type(u32)`:

| Tag | Type |
|-----|------|
| 0–7 | (existing: i8..u64) |
| 8 | Bool |
| 9 | Unit |
| 10 | Error |
| 11 | Never |
| 12 | ComptimeType |
| 13 | ComptimeStr |
| **14** | **I128** |
| **15** | **U128** |
| **16** | **Isize** |
| **17** | **Usize** |
| **18** | **F16** |
| **19** | **F32** |
| **20** | **F64** |
| **21** | **F128** |
| **22** | **ComptimeInt** |

This requires updating:
- `TypeKind` enum
- `Type` constants and `kind()`/`try_kind()` dispatch
- `is_integer()` (now 0–17 includes 128-bit and pointer-sized)
- New `is_float()` helper (18–21)
- New `is_numeric()` helper (`is_integer() || is_float()`)
- `is_signed()` (now includes I128=14, Isize=16, and all floats)
- `is_64_bit()` (add U64, I64, Isize/Usize on 64-bit targets, F64)
- `is_comptime_int()` helper (tag 22), mirroring `is_comptime_str()`
- `is_copy()` (ComptimeInt is Copy, like ComptimeStr)

### Arithmetic semantics

**Integers (i128/u128/isize/usize)**: Follow existing Gruel semantics — overflow panics at runtime (both signed and unsigned). All existing integer operators (`+`, `-`, `*`, `/`, `%`, `<<`, `>>`, `&`, `|`, `^`) work on these types.

**Floating-point (f16/f32/f64/f128)**: Support `+`, `-`, `*`, `/`, `%` (remainder), unary `-`. Do **not** support bitwise operators (`&`, `|`, `^`, `<<`, `>>`).

**Floating-point comparison**: `==`, `!=`, `<`, `>`, `<=`, `>=` use IEEE 754 semantics (NaN != NaN, NaN comparisons return false). No special NaN-handling syntax initially.

**Floating-point overflow**: Follows IEEE 754 — overflows produce infinity, not a panic. This differs from integer overflow behavior. Division by zero produces infinity (not a panic).

**f16 precision note**: `f16` has very limited precision (about 3 decimal digits, range ±65504). It is primarily a storage/interchange format for ML and GPU workloads. Arithmetic on `f16` is emitted as LLVM `half` operations — on targets without hardware f16 support, LLVM will promote to f32 for computation and convert back.

### Casting

Extend `@intCast` or introduce a more general `@cast` intrinsic for:
- Integer ↔ integer (existing, extended to 128-bit and pointer-sized)
- Float → float (`f16` ↔ `f32` ↔ `f64` ↔ `f128`)
- Integer → float
- Float → integer (truncates toward zero; panics if value is NaN or out of range of the target integer type, matching Rust's checked `as` behavior since 1.45)

No implicit numeric coercions — all cross-type conversions require explicit casting. This is consistent with Gruel's existing strictness.

### Target-dependent `isize`/`usize`

`isize` and `usize` are pointer-sized: 32 bits on 32-bit targets, 64 bits on 64-bit targets. This is determined at compile time from `gruel-target`.

- `isize`/`usize` are distinct types from `i32`/`i64` — no implicit conversion
- Array indexing will eventually require `usize` (future work, not part of this ADR)
- `@intCast` works between `isize`/`usize` and fixed-width integers

## Implementation Phases

### Phase 1: 128-bit integers (i128/u128)

- [x] Add `I128`/`U128` to `TypeKind`, `Type` constants (tags 8, 9 — shifted Bool+ up by 2)
- [x] Update all `is_integer()`, `is_signed()`, `is_unsigned()`, `is_copy()`, `literal_fits()`, `negated_literal_fits()` helpers
- [x] Add `i128`/`u128` tokens to lexer (`LogosTokenKind`)
- [x] Update parser type parsing to recognize `i128`/`u128`
- [x] Update RIR type resolution (inference `resolve_type_name` + sema `resolve_type`)
- [x] Update Sema to allow arithmetic/comparison on i128/u128
- [x] Update `@intCast` to handle 128-bit types
- [x] Add LLVM codegen: `ctx.i128_type()`, extend arithmetic/comparison emission
- [x] Update `type_byte_size` and `type_alignment` (16 bytes, 8-byte aligned on aarch64)
- [x] Add spec section 3.1 paragraphs for i128/u128 (3.1:21, 3.1:22)
- [x] Add spec tests (15 tests covering basic ops, casting, comparison, function calls, preview gate)
- [ ] **Deferred**: i128/u128 division/remainder (linker issue with compiler_builtins)
- [ ] **Deferred**: Overflow panic tests (lexer can't parse literals > i64::MAX)
- [x] Preview-gated via `--preview extended_numeric_types`

### Phase 2: Pointer-sized integers (isize/usize)

- [ ] Add `Isize`/`Usize` to `TypeKind`, `Type` constants (tags 16, 17)
- [ ] Update all type helper methods
- [ ] Add `isize`/`usize` tokens to lexer
- [ ] Update parser type parsing
- [ ] Update RIR and Sema for arithmetic/comparison
- [ ] Update `@intCast` to handle isize/usize
- [ ] Add LLVM codegen: map to `i32`/`i64` based on `gruel-target` pointer width
- [ ] Update `type_byte_size` and `type_alignment` (target-dependent)
- [ ] Add spec section 3.1 paragraphs for isize/usize
- [ ] Add spec tests

### Phase 3: Floating-point types (f16/f32/f64/f128) — lexer and type system

- [ ] Add `F16`/`F32`/`F64`/`F128` to `TypeKind`, `Type` constants (tags 18–21)
- [ ] Add `is_float()`, `is_numeric()` helpers
- [ ] Update `is_signed()`, `is_copy()`, etc.
- [ ] Add `f16`/`f32`/`f64`/`f128` type keyword tokens to lexer
- [ ] Add floating-point literal token: regex `[0-9]+\.[0-9]+([eE][+-]?[0-9]+)?` and `[0-9]+[eE][+-]?[0-9]+`
- [ ] Handle parser ambiguity: `42.method()` vs `42.0` (lookahead after `.` — if followed by digit, it's a float literal; if followed by identifier, it's a method call)
- [ ] Update parser type parsing to recognize `f16`/`f32`/`f64`/`f128`
- [ ] Add `Float(f64)` literal variant to RIR and AIR instruction data (store all float literals as f64, narrow to target width during codegen)

### Phase 4: Floating-point codegen and semantics

- [ ] Add LLVM codegen: `ctx.f16_type()`, `ctx.f32_type()`, `ctx.f64_type()`, `ctx.f128_type()`
- [ ] Emit float arithmetic: `fadd`, `fsub`, `fmul`, `fdiv`, `frem` for all four widths
- [ ] Emit float comparisons: `fcmp` with ordered predicates (OEQ, ONE, OLT, OGT, OLE, OGE)
- [ ] Emit float negation: `fneg`
- [ ] Reject bitwise operators on float types in Sema
- [ ] Update `type_byte_size` and `type_alignment`: f16 (2/2), f32 (4/4), f64 (8/8), f128 (16/16)
- [ ] Extend `@intCast` or add `@cast` for float↔int and float↔float conversions (fptrunc/fpext)
- [ ] Add spec section 3.5 for floating-point types
- [ ] Add spec tests for float arithmetic, comparison, casting, edge cases (NaN, infinity, negative zero)
- [ ] Add spec tests for float literal parsing
- [ ] Add spec tests for f16 range limits and f128 precision

### Phase 5: Compile-time integer type (comptime_int)

- [ ] Add `ComptimeInt` to `TypeKind`, `Type` constant (tag 22)
- [ ] Add `is_comptime_int()` helper, update `is_copy()`, `is_valid_encoding()`
- [ ] Mirror `ComptimeStr` handling: cannot be interned in type pool, not a runtime type
- [ ] Update comptime interpreter to produce `ComptimeInt` for integer expressions in comptime blocks
- [ ] Implement coercion from `comptime_int` to any integer type (with compile-time range validation)
- [ ] Update `Display`/`Debug`/`name()` to return `"comptime_int"`
- [ ] Add spec tests for comptime_int usage and coercion

### Phase 6: Polish and edge cases

- [ ] Update `Display`/`Debug` impls for new types
- [ ] Update fuzz targets to generate programs with new types
- [ ] Update comptime evaluation to use `i128` internally (currently uses `i64` — straightforward promotion)
- [ ] Decide comptime float evaluation strategy (run at compile time or defer to runtime)
- [ ] Ensure error messages mention new types correctly
- [ ] Update `EnumDef::discriminant_type` if needed for very large enums

## Consequences

### Positive

- Full numeric parity with Rust and beyond — `f16`/`f128` match Zig's coverage
- `usize` unlocks portable array indexing (future work)
- Floating-point enables scientific/graphics/game workloads
- `f16` enables ML inference and GPU interop without external conversion
- `f128` enables high-precision scientific computation
- 128-bit integers enable crypto and UUID operations

### Negative

- Increases compiler complexity (more type variants to handle in every match)
- Float semantics are inherently complex (NaN propagation, comparison edge cases)
- `isize`/`usize` introduce target-dependency into the type system
- Comptime evaluation needs promotion from `i64` to `i128` internally (straightforward)
- 128-bit integer support may have performance implications on some platforms (not all CPUs have native i128)
- `f16` has no hardware support on most non-GPU targets — LLVM emits software promotion to f32
- `f128` has no hardware support on x86-64 — LLVM emits software library calls (slow)

### Neutral

- The Type encoding scheme has plenty of room (tags 14–22 are unused)
- LLVM handles all the heavy lifting for code generation
- No syntax changes beyond new type keywords and float literals

## Open Questions

1. **Float literal suffixes**: Should we support `3.14f32` suffix syntax, or rely solely on type inference from annotations? (Current decision: no suffixes, use annotations.)

2. **NaN boxing or special NaN handling**: Should Gruel expose `NaN`/`Infinity` as named constants, or require `0.0 / 0.0`-style construction? (Suggest: add `f32::NAN`, `f64::INFINITY` etc. as associated constants in a future ADR.)

3. **`isize`/`usize` as array index type**: Should array indexing require `usize`? Current arrays use integer expressions. This is a semantic change that affects existing code and should be a separate ADR.

4. **128-bit alignment**: Should `i128`/`u128` be 16-byte aligned (matching some platforms) or 8-byte aligned (matching Rust on x86-64)? Defer to LLVM's target data layout.

5. **Comptime float**: Should float expressions be evaluable at comptime? This adds complexity (f64 arithmetic at compile time). Could defer and only allow float literals, not comptime float math.

## Future Work

- `char` type (Unicode scalar value, stored as u32)
- Float-specific intrinsics (`@sqrt`, `@sin`, `@cos`, etc.) or a `math` module
- Functions that check if a float is `NaN` or `Infinity`
- `usize` as the required array index type
- Wrapping/saturating arithmetic operators or methods for integers

## References

- [Rust Reference: Numeric Types](https://doc.rust-lang.org/reference/types/numeric.html)
- [IEEE 754-2019](https://standards.ieee.org/ieee/754/6210/)
- [LLVM Language Reference: Integer Type](https://llvm.org/docs/LangRef.html#integer-type)
- [LLVM Language Reference: Floating-Point Types](https://llvm.org/docs/LangRef.html#floating-point-types)
- [ADR-0024: Type Intern Pool](0024-type-intern-pool-revised.md) — Type encoding scheme this extends

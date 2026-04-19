---
id: 0040
title: Comptime Interpreter Expansion and Differential Fuzzing
status: proposal
tags: [compiler, comptime, fuzzing, testing]
feature-flag: comptime-expansion
created: 2026-04-19
accepted:
implemented:
spec-sections: ["4.14"]
superseded-by:
---

# ADR-0040: Comptime Interpreter Expansion and Differential Fuzzing

## Status

Proposal

## Summary

Expand the comptime interpreter to support nearly all pure language constructs — mutation of struct fields and array elements, enum construction and pattern matching, generic function calls, integer casts, and type intrinsics — closing the gap between what comptime can evaluate and what the LLVM backend can execute. Then build a differential fuzzer that generates programs, runs them through both paths, and asserts identical results, providing ongoing confidence that the interpreter and codegen agree on Gruel's semantics.

## Context

### Current comptime state

ADR-0033 replaced the old expression-only constant folder with a proper AIR-level interpreter that has a call stack, mutable locals, and a comptime heap. The interpreter currently handles:

- Literals (integer, bool, unit)
- All arithmetic, comparison, logical, and bitwise operations
- Mutable `let` bindings and assignment
- `if`/`else`, `while`, `loop`, `break`, `continue`, `return`
- Non-generic function calls (up to 64 frames deep, 1M step budget)
- Struct construction and field read (`FieldGet`)
- Array construction and index read (`IndexGet`)

### What's missing

| Feature | RIR instruction(s) | Why it matters |
|---|---|---|
| Struct field mutation | `FieldSet` | Can't modify struct fields in comptime loops |
| Array element mutation | `IndexSet` | Can't build arrays element-by-element |
| Enum values | `EnumVariant`, `EnumStructVariant` | Can't use enums in comptime at all |
| Pattern matching | `Match` | Can't branch on enum variants or integer patterns |
| Generic function calls | `Call` (with `is_generic`) | Most utility functions are generic |
| Integer casts | `Intrinsic("intCast")` | Can't convert between integer widths |
| Type intrinsics | `TypeIntrinsic("size_of", "align_of")` | Can't query type layout at comptime |
| Struct destructuring | `StructDestructure` | Can't unpack structs in comptime bindings |
| Method calls | `MethodCall` | Can't call methods on comptime structs |
| Associated function calls | `AssocFnCall` | Can't call `Type::func()` at comptime |
| Compound assignment | `Assign` on `FieldSet`/`IndexSet` | Can't `point.x = point.x + 1` |

None of these are conceptually hard — the execution model (heap-indexed composites, call frames, step budget) already supports them. They were deferred in ADR-0033 because the LLVM migration was the priority.

### Why differential fuzzing

The comptime interpreter and the LLVM codegen are two independent implementations of Gruel's semantics. Any divergence is a bug in one or the other. A differential fuzzer that generates valid Gruel programs, runs them through both comptime evaluation and runtime execution, and compares `@dbg` output provides:

1. **Continuous correctness validation** — catches interpreter bugs that spec tests miss
2. **Regression protection** — as either the interpreter or codegen evolves
3. **Spec coverage** — fuzzer-generated programs exercise corner cases humans wouldn't write

The existing `structured_compiler` fuzz target generates valid Gruel programs via the `arbitrary` crate. Extending this to also exercise the comptime path and compare results is a natural evolution.

### Design principles

**No comptime pointers.** Gruel's memory safety model doesn't have raw pointers in normal code. The heap-index approach (`ConstValue::Struct(u32)`, `ConstValue::Array(u32)`) gives comptime composite values without needing a virtual address space, pointer validity tracking, or borrow checking inside the interpreter. This is a deliberate simplification over Zig.

**No comptime strings (yet).** String operations involve runtime allocation and are not pure computation. Comptime strings would require either a comptime allocator or a `ConstValue::String` variant with owned data. This is future work, outside the scope of this ADR.

**No comptime I/O — except `@dbg`.** Functions with side effects (`@readLine`, `@panic`) remain compile errors in comptime context. `@dbg` is the exception: it is supported as a pure formatting operation that writes to an internal buffer (not stdout) during comptime evaluation. This enables differential fuzzing by providing a comparison surface between comptime and runtime execution.

## Decision

### Phase 1: Mutation operations

Add `FieldSet` and `IndexSet` support to `evaluate_comptime_inst`. Both require locating the heap item and modifying it in-place.

#### FieldSet

```rust
InstData::FieldSet { base, field, value } => {
    // base must be a VarRef to a local holding a ConstValue::Struct(heap_idx).
    // Resolve the field index from the struct definition, evaluate the value,
    // and update the heap item in-place.
}
```

The key subtlety: `base` in RIR is a `VarRef`, not a value. The interpreter must resolve it to a local variable, extract the heap index, mutate the heap, and leave the local unchanged (it still points to the same heap slot). This matches the runtime semantics where `FieldSet` is a store through a pointer.

#### IndexSet

Same pattern as `FieldSet` — resolve the base variable to its `ConstValue::Array(heap_idx)`, evaluate the index and value, bounds-check, and update the element in the heap.

### Phase 2: Enum support

#### ConstValue extension

```rust
pub enum ConstValue {
    // ...existing...
    /// Enum variant with no data (e.g., `Color::Red`).
    /// Stores the enum id and variant index.
    EnumVariant { enum_id: EnumId, variant_idx: u32 },
    /// Enum variant with tuple data (e.g., `Option::Some(42)`).
    /// Data fields are stored on the comptime heap.
    EnumData { enum_id: EnumId, variant_idx: u32, heap_idx: u32 },
    /// Enum variant with struct data (e.g., `Shape::Rect { w: 10, h: 20 }`).
    /// Fields are stored on the comptime heap.
    EnumStruct { enum_id: EnumId, variant_idx: u32, heap_idx: u32 },
}
```

`ConstValue` must remain `Copy`, so data goes on the comptime heap. Since enum variants already carry an `EnumId` and variant index at the type level, we store those directly in the `ConstValue` and put any associated data on the heap.

#### ComptimeHeapItem extension

```rust
pub enum ComptimeHeapItem {
    Struct { struct_id: StructId, fields: Vec<ConstValue> },
    Array(Vec<ConstValue>),
    EnumData(Vec<ConstValue>),     // tuple variant fields
    EnumStruct(Vec<ConstValue>),   // struct variant fields, in declaration order
}
```

#### Interpreter additions

- `EnumVariant { type_name, variant, .. }` → resolve enum and variant index, return `ConstValue::EnumVariant { enum_id, variant_idx }`
- `EnumStructVariant { type_name, variant, fields_start, fields_len, .. }` → evaluate field values, allocate on heap, return `ConstValue::EnumStruct { ... }`

### Phase 3: Pattern matching

Add `Match` support to the interpreter. Pattern matching requires:

1. Evaluate the scrutinee to a `ConstValue`
2. Iterate arms in order, testing each pattern against the scrutinee
3. On match, bind any captured variables into `locals` and evaluate the arm body
4. Return the value of the matched arm (or error if no arm matches — should not happen after exhaustiveness checking)

Pattern types to support:

| Pattern | Matching logic |
|---|---|
| `Wildcard` | Always matches, binds nothing |
| `Int(n)` | Matches `ConstValue::Integer(m)` where `n == m` |
| `Bool(b)` | Matches `ConstValue::Bool(c)` where `b == c` |
| `Path { type_name, variant }` | Matches `ConstValue::EnumVariant` with same enum+variant |
| `DataVariant { type_name, variant, bindings }` | Matches `ConstValue::EnumData`, binds tuple fields |
| `StructVariant { type_name, variant, field_bindings }` | Matches `ConstValue::EnumStruct`, binds struct fields |

### Phase 4: Generic function calls

Currently, `evaluate_comptime_inst` bails on `fn_info.is_generic` with a `not_const` error. The fix:

1. Evaluate all comptime arguments (type and value args) before the call
2. Look up or create the monomorphized specialization using the existing `specialize` infrastructure
3. The specialized function is non-generic — its RIR body has concrete types substituted
4. Push a new call frame and execute the specialized body as usual

This requires that the specialization infrastructure can be invoked from within the comptime interpreter, which means `evaluate_comptime_inst` needs access to the specialization cache and the ability to trigger on-demand analysis of callees.

The key concern is re-entrancy: comptime evaluation happens during sema, and specialization also happens during sema. The existing on-demand analysis pattern (used for non-generic comptime calls) already handles this — extending it to generic calls follows the same structure but adds the specialization lookup step.

### Phase 5: Remaining operations

#### Integer casts (`@intCast`)

When evaluating `Intrinsic { name: "intCast", .. }` in comptime context, perform the cast on the `ConstValue::Integer(i64)` value. Since comptime integers are `i64`, the cast is a range check: verify the value fits in the target type's range, then return it as `ConstValue::Integer`. Overflow is a compile error.

#### Type intrinsics (`@size_of`, `@align_of`)

`TypeIntrinsic { name, type_arg }` can be evaluated at comptime by resolving the type and computing the layout. These are already computed as constants during sema (they emit `AirInstData::Const`), so the interpreter just needs to do the same calculation: resolve the type, compute slot count, return `ConstValue::Integer(slot_count * 8)` for `size_of` or the alignment value for `align_of`.

#### Struct destructuring

`StructDestructure { type_name, fields_start, fields_len, init }` evaluates the init expression to a `ConstValue::Struct(heap_idx)`, then binds each named field into `locals`. Fields with wildcard bindings are skipped.

#### Method calls

`MethodCall { receiver, method, args_start, args_len }` — evaluate the receiver, look up the method on the receiver's struct type, and invoke it as a function call with `self` bound to the receiver value. This reuses the function call machinery from Phase 1c of ADR-0033.

For methods that mutate `self` (inout receivers), the interpreter must write the modified `self` back to the local variable after the call returns, similar to `FieldSet` semantics.

#### Associated function calls

`AssocFnCall { type_name, function, args_start, args_len }` — resolve the type and function, then invoke as a regular function call. No receiver binding needed since associated functions don't take `self`.

### Phase 6: Differential fuzzer

#### Architecture

```
┌─────────────────────────────────────┐
│  Structured program generator       │
│  (extends gruel-fuzz GruelProgram)  │
└──────────────┬──────────────────────┘
               │ generates source: String
               ▼
┌──────────────────────────────────────┐
│  Wrapper: wraps body in two forms    │
│                                      │
│  Form A: comptime {                  │
│    const RESULT: i32 = comptime {    │
│      <generated body>                │
│    };                                │
│    fn main() -> i32 { RESULT }       │
│                                      │
│  Form B: runtime                     │
│    fn main() -> i32 {                │
│      <generated body>                │
│    }                                 │
└──────┬──────────────────┬────────────┘
       │                  │
       ▼                  ▼
  compile_frontend()   compile + link + run
       │                  │
       ▼                  ▼
  ConstValue from      exit code from
  comptime eval        process execution
       │                  │
       └──────┬───────────┘
              ▼
        assert_eq!(comptime_result, runtime_exit_code)
```

#### Generator constraints

The program generator must produce programs that are valid in both comptime and runtime contexts. This means:

- **No I/O intrinsics** (`@readLine`) — but `@dbg` is allowed (it's the comparison mechanism)
- **No string operations** (not supported in comptime)
- **No extern calls**
- **No infinite loops** (step budget will catch these in comptime, but runtime would hang)
- **Deterministic** — no `@randomU32` etc.

The generator should produce programs using the constructs supported by earlier phases: arithmetic, control flow, function calls (including generic), structs, arrays, enums, and pattern matching. Programs should use `@dbg` calls to emit intermediate and final values for comparison.

#### Comparison mechanism: `@dbg`-based stdout diffing

Rather than comparing exit codes (which are limited to 0–255), the fuzzer uses `@dbg` as a serialization mechanism. Both paths produce lines of text — the fuzzer compares them.

**Runtime path**: `@dbg` calls the existing runtime functions (`__gruel_dbg_i64`, `__gruel_dbg_u64`, `__gruel_dbg_bool`) which print to stdout. The fuzzer captures stdout after execution.

**Comptime path**: The interpreter handles `@dbg` as a special-cased intrinsic. Instead of rejecting it as a side effect, it formats the `ConstValue` into a `comptime_dbg_output: Vec<String>` buffer on `Sema`. After compilation, the fuzzer reads this buffer.

The comptime `@dbg` handler must format values identically to the runtime functions. The key subtlety is signed vs unsigned: the runtime dispatches to `__gruel_dbg_i64` or `__gruel_dbg_u64` based on the argument's type, but `ConstValue::Integer(i64)` doesn't carry signedness. The interpreter must resolve the argument's type from the RIR to choose the correct format (signed decimal vs unsigned decimal).

This approach gives the fuzzer:
- Full integer range comparison (not truncated to 0–255)
- Multiple comparison points per program (each `@dbg` call is a check)
- Intermediate state visibility (can `@dbg` values inside loops, branches, etc.)

#### Fuzz target

```rust
// fuzz/fuzz_targets/comptime_differential.rs
#![no_main]
use libfuzzer_sys::fuzz_target;
use gruel_fuzz::ComptimeProgram;  // New generator type

fuzz_target!(|prog: ComptimeProgram| {
    let source = prog.source();

    // Path A: comptime evaluation (dbg output collected in buffer)
    let comptime_source = format!(
        "const _: () = comptime {{\n{}\n}};\nfn main() -> i32 {{ 0 }}",
        source
    );
    let comptime_dbg = match gruel_compiler::compile_frontend(&comptime_source) {
        Ok(state) => state.comptime_dbg_output().join("\n"),
        Err(_) => return,  // Skip programs that don't compile
    };

    // Path B: runtime execution (dbg output captured from stdout)
    let runtime_source = format!("fn main() -> i32 {{\n{}\n0\n}}", source);
    let runtime_dbg = match compile_and_run(&runtime_source) {
        Some(stdout) => stdout,
        None => return,  // Skip if compilation or execution fails
    };

    // Compare dbg output line by line
    assert_eq!(comptime_dbg, runtime_dbg,
        "comptime/runtime divergence for program:\n{}", source);
});
```

#### Integration with existing fuzz infrastructure

The new `ComptimeProgram` generator extends the existing `GruelProgram` generator from `gruel-fuzz` with additional constraints (no I/O, no strings, deterministic). It reuses the same `Arbitrary` trait implementation patterns.

The `compile_and_run` helper compiles to a temporary binary and executes it, capturing the exit code. This is similar to what the spec test runner does.

## Implementation Phases

- [x] **Phase 1: Mutation operations** — Add `FieldSet` and `IndexSet` to the comptime interpreter. Add spec tests for comptime struct field mutation and array element mutation.
- [x] **Phase 2: Enum support** — Add `ConstValue::EnumVariant/EnumData/EnumStruct` variants, `ComptimeHeapItem::EnumData/EnumStruct`, and interpret `EnumVariant`/`EnumStructVariant` instructions. Add spec tests for comptime enum construction.
- [x] **Phase 3: Pattern matching** — Add `Match` support to the interpreter with all pattern types (Wildcard, Int, Bool, Path, DataVariant, StructVariant). Add spec tests for comptime pattern matching.
- [x] **Phase 4: Generic function calls** — Extend comptime `Call` handling to support generic functions by invoking the specialization infrastructure on-demand. Add spec tests for comptime calls to generic functions.
- [ ] **Phase 5: Remaining operations** — Add `@intCast`, `@size_of`/`@align_of`, `StructDestructure`, `MethodCall`, and `AssocFnCall` support. Add comptime `@dbg` handler: special-case the `dbg` intrinsic in `evaluate_comptime_inst` to format `ConstValue` into a `comptime_dbg_output` buffer on `Sema` (resolving argument type for signed/unsigned formatting), and expose the buffer through `CompileState`. Add spec tests for each.
- [ ] **Phase 6: Differential fuzzer** — Add `ComptimeProgram` generator to `gruel-fuzz` (produces programs with `@dbg` calls, no I/O, no strings, deterministic), add `comptime_differential` fuzz target that compares comptime `@dbg` buffer output against runtime stdout, add CI integration.

## Consequences

### Positive

- Comptime becomes useful for real metaprogramming — users can write comptime functions that manipulate structs, match on enums, and call generic utilities
- Differential fuzzer provides ongoing correctness assurance that spec tests alone cannot
- Closes the most commonly-hit comptime gaps (mutation, enums, generics) that users encounter when trying to write nontrivial comptime code
- Each phase is independently valuable — partial completion still improves comptime

### Negative

- Interpreter complexity grows significantly (~6 new instruction handlers, enum value representation, match evaluation)
- Generic function calls in comptime increase compile times for programs that heavily use comptime generics
- Differential fuzzer requires compiling and executing binaries during fuzz runs, which is slow compared to frontend-only fuzzing
- New `ConstValue` variants (3 enum-related) increase the size of the enum, though it remains `Copy` (all data stays on the heap)

### Neutral

- No new language syntax — all constructs already exist, they just become usable in comptime context
- No changes to the LLVM codegen — comptime is fully resolved before CFG construction
- The step budget and call depth limits remain unchanged

## Open Questions

1. **Enum variant `ConstValue` representation**: Three separate variants (`EnumVariant`, `EnumData`, `EnumStruct`) vs. a single `Enum { enum_id, variant_idx, data: Option<u32> }` where `data` is an optional heap index? The three-variant approach is more explicit but increases the enum size. A single variant with an optional heap index would be more compact.

2. **Generic call re-entrancy**: The specialization pass currently runs as a separate phase after initial sema. Invoking it from within `evaluate_comptime_inst` requires careful handling to avoid infinite specialization loops. Should we add a specialization cache check, or rely on the existing call depth limit?

3. **Method call receiver mutation**: For `inout self` methods called at comptime, the interpreter must write back the mutated `self` to the caller's local. Should this follow the same heap-mutation pattern as `FieldSet`, or should it create a new heap entry (copy-on-call)?

## Future Work

- **`comptime_str` type**: A comptime-only string type, following the `comptime_` naming convention established by `comptime_int` and `comptime_float` (ADR-0025). Unlike the runtime `String` type (which is a synthetic struct backed by `{ptr, len, cap}` and FFI methods in `gruel-runtime`), `comptime_str` would be a pure comptime value — an owned Rust `String` inside the interpreter, with its own method set (`len`, `contains`, `starts_with`, `split`, etc.) implemented directly in the interpreter rather than through runtime functions. This avoids the dual-implementation problem: runtime `String` keeps its existing semantics untouched, and `comptime_str` methods can be richer (e.g., `split`, `trim`) without requiring runtime implementations. Like `type`, `comptime_str` would be rejected in runtime positions. A future `to_static` method could materialize a `comptime_str` into a runtime `String` backed by `.rodata` data (using the same mechanism as string literals, which already have `cap = 0`), bridging the two worlds when needed.
- **Comptime reflection**: `@typeInfo`-style intrinsics returning struct/enum metadata as comptime values
- **Comptime allocator**: Heap allocations that persist into the compiled binary as static data
- **`inline for`**: Loop unrolling over comptime-known collections
- **`@compileError` / `@compileLog`**: User-controlled compile-time diagnostics

## References

- [ADR-0025: Compile-Time Execution](0025-comptime.md) — original comptime design
- [ADR-0033: LLVM Backend and Comptime Interpreter](0033-llvm-backend-and-comptime-interpreter.md) — current interpreter implementation
- [Zig Language Reference: comptime](https://ziglang.org/documentation/master/#comptime) — inspiration for comptime model
- [Csmith](https://embed.cs.utah.edu/csmith/) — differential fuzzing of C compilers, inspiration for the approach

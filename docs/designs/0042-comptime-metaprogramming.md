---
id: 0042
title: Comptime Metaprogramming — Diagnostics, Strings, Reflection, and comptime_unroll For
status: proposal
tags: [compiler, comptime, metaprogramming, type-system]
feature-flag: comptime-meta
created: 2026-04-19
accepted:
implemented:
spec-sections: ["4.14"]
superseded-by:
---

# ADR-0042: Comptime Metaprogramming — Diagnostics, Strings, Reflection, and comptime_unroll For

## Status

Proposal

## Summary

Complete the comptime metaprogramming story by adding four capabilities identified as future work in ADR-0040: user-controlled compile-time diagnostics (`@compileError`, `@compileLog`), a comptime-only string type (`comptime_str`), compile-time type reflection (`@typeInfo`), and compile-time loop unrolling (`comptime_unroll for`). Together, these enable Gruel users to write sophisticated type-safe code generators and generic abstractions — the kind of metaprogramming that Zig's comptime model is known for. The comptime allocator (materializing comptime heap into the binary's static data) is explicitly deferred to a future ADR.

## Context

### Where comptime stands today

ADR-0025 introduced the `comptime` keyword, type parameters, and monomorphization. ADR-0033 replaced the expression-only constant folder with a proper AIR-level interpreter (call stack, mutable locals, comptime heap). ADR-0040 closed the remaining operational gaps — mutation, enums, pattern matching, generic calls, method calls, and `@dbg`. A differential fuzzer validates interpreter-vs-codegen agreement.

The interpreter now supports nearly every pure language construct. What's missing is not *evaluation* capability but *metaprogramming* capability — the tools that let comptime code inspect types, generate diagnostics, manipulate text, and unroll loops over compile-time data.

### What's missing

| Feature | What it enables | Zig equivalent |
|---------|----------------|----------------|
| `@compileError` / `@compileLog` | User-defined compile errors and debug logging | `@compileError`, `@compileLog` |
| `comptime_str` | String manipulation at compile time (field names, error messages) | Comptime slices (built-in) |
| `@typeInfo` | Inspecting struct fields, enum variants, function signatures | `@typeInfo` |
| `comptime_unroll for` | Unrolling loops over comptime-known collections | `inline for` |

These features have strong dependencies on each other:

```
@compileError/@compileLog  (standalone)
        │
        ▼
   comptime_str  ←── string literals in comptime context
        │
        ▼
    @typeInfo  ←── returns comptime_str for field/variant names
        │
        ▼
   comptime_unroll for  ←── iterating over @typeInfo results
```

`@compileError`/`@compileLog` are standalone. `comptime_str` is a prerequisite for `@typeInfo` (field names are strings). `@typeInfo` motivates `comptime_unroll for` (iterating over struct fields). Each phase is independently useful, but the full power emerges when combined.

### Why not the comptime allocator?

The comptime allocator (materializing comptime heap items into the compiled binary as static data) crosses the interpreter/codegen boundary — it requires LLVM global generation, lifetime semantics for frozen data, and decisions about mutability of materialized values. It deserves its own ADR and does not block the metaprogramming features in this one.

### Design principles (carried forward from ADR-0040)

- **No comptime pointers.** Heap-indexed values (`ConstValue::Struct(u32)`) remain the representation for composites.
- **`ConstValue` stays `Copy`.** All variable-size data lives on the comptime heap; `ConstValue` holds only fixed-size tags and indices.
- **Same syntax where possible.** New constructs reuse existing syntax patterns. `comptime_unroll for` introduces a new keyword (`comptime_unroll`) that clearly communicates the construct belongs to the comptime family and that it unrolls — generating runtime code from compile-time data.

## Decision

### Phase 1: `@compileError` and `@compileLog`

Two new intrinsics that give comptime code control over compiler diagnostics.

#### `@compileError(message)`

Emits a compile error with a user-defined message. The message must be a string literal (Phase 1) or a `comptime_str` value (after Phase 2).

```gruel
fn Matrix(comptime rows: i32, comptime cols: i32) -> type {
    if rows <= 0 || cols <= 0 {
        @compileError("Matrix dimensions must be positive");
    }
    struct { data: [i32; rows * cols] }
}
```

**Semantics:**
- Evaluated during comptime interpretation, not during parsing
- Unreachable `@compileError` calls are never evaluated (dead code in branches is fine)
- The message becomes the primary error text in the diagnostic
- Type: `@compileError` has type `!` (never) — it terminates compilation of the current comptime block

**Interpreter implementation:**

```rust
// In evaluate_comptime_inst, Intrinsic handler:
if name == self.known.compile_error {
    let msg = self.evaluate_comptime_string_arg(arg_refs[0], locals, ctx, outer_span)?;
    return Err(CompileError::new(
        ErrorKind::ComptimeUserError(msg),
        inst_span,
    ));
}
```

The `evaluate_comptime_string_arg` helper resolves the argument. In Phase 1, it only accepts `StringConst` instructions (string literals). In Phase 2, it also accepts `ConstValue::ComptimeStr` values.

#### `@compileLog(args...)`

Emits a compile-time log message. Unlike `@compileError`, it does not stop compilation — it prints during the compilation process. Useful for debugging comptime logic.

```gruel
fn compute(comptime n: i32) -> type {
    @compileLog("computing with n =", n);
    // ...
}
```

**Semantics:**
- Variadic: accepts any number of arguments of any comptime-evaluable type
- Each argument is formatted (integers as decimal, bools as `true`/`false`, types as their name, `comptime_str` as their content)
- Output goes to stderr with a `comptime log:` prefix, similar to Zig
- The result type is `()` (unit) — it's a statement, not an expression
- A program that compiles successfully but contains `@compileLog` calls emits a warning ("comptime log present — remove before release"), preventing accidental commit of debug logging

**Interpreter implementation:**

```rust
if name == self.known.compile_log {
    let mut parts = Vec::new();
    for &arg_ref in arg_refs {
        let val = self.evaluate_comptime_inst(arg_ref, locals, ctx, outer_span)?;
        parts.push(self.format_const_value(val));
    }
    let msg = parts.join(" ");
    eprintln!("comptime log: {}", msg);
    // Also store in a buffer for warning generation
    self.comptime_log_output.push((msg, inst_span));
    return Ok(ConstValue::Unit);
}
```

After sema completes, if `comptime_log_output` is non-empty, emit a warning for each entry.

#### RIR changes

No new RIR instructions needed. `@compileError` and `@compileLog` are parsed as `Intrinsic` instructions (same as `@dbg`, `@intCast`). The names `compileError` and `compileLog` are registered in `KnownSymbols`.

#### Error infrastructure

Add `ErrorKind::ComptimeUserError(String)` to `gruel-error` for user-defined compile errors.

### Phase 2: `comptime_str` type

A comptime-only string type for manipulating text at compile time. Unlike the runtime `String` type (which is a synthetic struct backed by `{ptr, len, cap}` and FFI methods in `gruel-runtime`), `comptime_str` is a pure interpreter value — an owned Rust `String` inside the comptime heap.

#### Type system

Add `Type::ComptimeStr` as a new type tag in the type intern pool, alongside `Type::ComptimeType`. Like `type`, `comptime_str` is rejected in runtime positions.

```gruel
fn greet(comptime name: comptime_str) -> type {
    @compileLog("building greeter for", name);
    struct {
        fn hello(self) -> i32 { 42 }
    }
}
```

#### ConstValue and heap

```rust
// New ConstValue variant
pub enum ConstValue {
    // ...existing...
    /// Index into comptime_heap for a comptime string value.
    ComptimeStr(u32),
}

// New ComptimeHeapItem variant
pub enum ComptimeHeapItem {
    // ...existing...
    /// A comptime string value.
    String(String),
}
```

#### String literal promotion

When a string literal (`StringConst`) appears in a comptime context, the interpreter promotes it to a `ConstValue::ComptimeStr`:

```rust
InstData::StringConst(symbol) => {
    let s = self.interner.resolve(&symbol).to_string();
    let idx = self.comptime_heap.len() as u32;
    self.comptime_heap.push(ComptimeHeapItem::String(s));
    Ok(ConstValue::ComptimeStr(idx))
}
```

This is the bridge: string literals are runtime values in normal code but become `comptime_str` values inside comptime blocks.

#### Methods

`comptime_str` methods are implemented directly in the interpreter, not through the synthetic struct / runtime function pattern. Each method is dispatched by name in a `MethodCall` handler for `comptime_str` receivers:

| Method | Signature | Description |
|--------|-----------|-------------|
| `len` | `fn len(self) -> i32` | Length in bytes |
| `is_empty` | `fn is_empty(self) -> bool` | Whether length is zero |
| `contains` | `fn contains(self, needle: comptime_str) -> bool` | Substring search |
| `starts_with` | `fn starts_with(self, prefix: comptime_str) -> bool` | Prefix check |
| `ends_with` | `fn ends_with(self, suffix: comptime_str) -> bool` | Suffix check |
| `eq` | `fn eq(self, other: comptime_str) -> bool` | Equality (`==` operator) |
| `ne` | `fn ne(self, other: comptime_str) -> bool` | Inequality (`!=` operator) |
| `lt` | `fn lt(self, other: comptime_str) -> bool` | Less than (`<` operator, lexicographic) |
| `le` | `fn le(self, other: comptime_str) -> bool` | Less or equal (`<=` operator, lexicographic) |
| `gt` | `fn gt(self, other: comptime_str) -> bool` | Greater than (`>` operator, lexicographic) |
| `ge` | `fn ge(self, other: comptime_str) -> bool` | Greater or equal (`>=` operator, lexicographic) |
| `concat` | `fn concat(self, other: comptime_str) -> comptime_str` | Concatenation |

**Deferred methods** (can be added incrementally, not gated):
- `trim`, `trim_start`, `trim_end` — whitespace trimming
- `to_upper`, `to_lower` — case conversion
- `split` — splitting into an array (requires comptime arrays of comptime_str)
- `slice` — substring extraction

The initial set is deliberately minimal. The most important use case for `comptime_str` in Phase 2 is *receiving* field names from `@typeInfo` (Phase 3) and *passing* them to `@compileError` / `@compileLog` (Phase 1). Rich string manipulation can be added as methods without further gating.

#### `@dbg` extension

Extend the comptime `@dbg` handler to format `ConstValue::ComptimeStr` values:

```rust
ConstValue::ComptimeStr(idx) => {
    let s = match &self.comptime_heap[idx as usize] {
        ComptimeHeapItem::String(s) => s.clone(),
        _ => unreachable!(),
    };
    format!("\"{}\"", s)  // Match runtime @dbg format for strings
}
```

#### `@compileError` / `@compileLog` extension

After Phase 2, `@compileError` and `@compileLog` accept `comptime_str` arguments in addition to string literals:

```gruel
fn check(comptime T: type) -> type {
    let info = @typeInfo(T);
    if info.fields.len == 0 {
        @compileError("type has no fields");
    }
    // ...
}
```

### Phase 3: `@typeInfo` and `@typeName`

Compile-time type reflection — inspecting the structure of types during comptime evaluation.

#### `@typeName(T)` — simple type name

Returns the name of a type as a `comptime_str`:

```gruel
fn debug_type(comptime T: type) -> comptime_str {
    @typeName(T)  // "i32", "MyStruct", "Option", etc.
}
```

**Implementation:** A new `TypeIntrinsic` handler in the comptime interpreter. Resolves the type, formats its name, allocates a `ComptimeStr` on the heap.

#### `@typeInfo(T)` — full type metadata

Returns a comptime struct describing the type's structure. The shape of the returned struct depends on the type's kind.

```gruel
fn inspect(comptime T: type) {
    let info = @typeInfo(T);
    @compileLog("type", @typeName(T), "has", info.fields.len, "fields");
}
```

#### `TypeKind` builtin enum

A new builtin enum for discriminating type kinds, injected via the existing `BuiltinEnumDef` infrastructure (same pattern as `Arch` and `Os`):

```rust
pub static TYPE_KIND_ENUM: BuiltinEnumDef = BuiltinEnumDef {
    name: "TypeKind",
    variants: &["Struct", "Enum", "Int", "Bool", "Unit", "Never", "Array"],
};
```

This enables `match` on type kind rather than string comparisons.

**Returned structures:**

For struct types:
```gruel
// @typeInfo(SomeStruct) returns:
struct {
    kind: TypeKind,             // TypeKind::Struct
    name: comptime_str,         // "SomeStruct"
    fields: [FieldInfo; N],     // fixed-size array
}

// Where FieldInfo is:
struct {
    name: comptime_str,         // "x", "y", etc.
    field_type: type,           // i32, bool, etc.
}
```

For enum types:
```gruel
// @typeInfo(SomeEnum) returns:
struct {
    kind: TypeKind,                 // TypeKind::Enum
    name: comptime_str,             // "SomeEnum"
    variants: [VariantInfo; N],     // fixed-size array
}

// Where VariantInfo is:
struct {
    name: comptime_str,             // "Red", "Some", etc.
    fields: [FieldInfo; M],         // empty for unit variants
}
```

For primitive types:
```gruel
// @typeInfo(i32) returns:
struct {
    kind: TypeKind,         // TypeKind::Int
    name: comptime_str,     // "i32"
    bits: i32,              // 32
    is_signed: bool,        // true
}
```

Users can then write:

```gruel
fn describe(comptime T: type) {
    let info = @typeInfo(T);
    match info.kind {
        TypeKind::Struct => @compileLog("struct with", info.fields.len, "fields"),
        TypeKind::Enum => @compileLog("enum with", info.variants.len, "variants"),
        TypeKind::Int => @compileLog("integer:", info.bits, "bits"),
        _ => @compileLog("other type"),
    }
}
```

**Implementation strategy:**

`@typeInfo` is a `TypeIntrinsic` that, during comptime evaluation, constructs anonymous struct types and values on the fly:

1. Resolve the type argument to a concrete `Type`
2. Match on its `TypeKind` (Struct, Enum, Int, Bool, etc.)
3. Create anonymous struct types for `FieldInfo`, `VariantInfo`, and the top-level info struct using the existing anonymous struct infrastructure from ADR-0025 Phase 4
4. Set the `kind` field to the appropriate `TypeKind` variant (a `ConstValue::EnumVariant`)
5. Allocate instances of these structs on the comptime heap with the appropriate field values
6. Return the `ConstValue::Struct(heap_idx)` pointing to the info struct

The anonymous struct types created by `@typeInfo` are cached per-type to avoid creating duplicate type definitions. A `HashMap<Type, StructId>` in `Sema` maps inspected types to their info struct types.

The key subtlety: `FieldInfo` structs contain `field_type: type` fields, which store `ConstValue::Type(ty)` values. This reuses the existing comptime type value infrastructure. Field name strings use `ConstValue::ComptimeStr`, which is why Phase 2 is a prerequisite.

### Phase 4: `comptime_unroll for`

Compile-time loop unrolling over comptime-known collections.

#### Syntax

```ebnf
comptime_unroll_for = "comptime_unroll" "for" ["mut"] identifier "in" expression "{" block "}" ;
```

The syntax mirrors regular `for` loops (no parentheses around the binding/iterable), prefixed with the `comptime_unroll` keyword.

```gruel
fn sum_fields(comptime T: type, val: T) -> i32 {
    let info = @typeInfo(T);
    let mut total: i32 = 0;
    comptime_unroll for field in info.fields {
        total = total + @field(val, field.name);
    }
    total
}
```

#### Semantics

1. The collection expression must be comptime-known (a comptime array or `@range` call with comptime-known arguments)
2. The loop is unrolled at compile time: one copy of the body per element
3. The loop variable is comptime within each unrolled iteration
4. The unrolled body is analyzed in a runtime context (it can reference runtime variables)

**Distinction from `for` inside `comptime` blocks:** Regular `for` loops work inside `comptime` blocks just like `while` and `loop` — the entire loop runs at compile time and produces a single comptime value. `comptime_unroll for` is fundamentally different: it produces *runtime* code (one copy of the body per iteration), while a `for` inside `comptime { }` produces a single *comptime* value. `comptime_unroll for` bridges comptime and runtime — it uses comptime data to generate runtime code.

```gruel
// Pure comptime — the for loop runs entirely at compile time, producing one value
let x = comptime {
    let mut sum = 0;
    for i in @range(10) { sum = sum + i; }
    sum  // 45
};

// comptime_unroll — uses comptime data to generate N copies of runtime code
fn sum_fields(comptime T: type, val: T) -> i32 {
    let mut total: i32 = 0;
    comptime_unroll for field in @typeInfo(T).fields {
        total = total + @field(val, field.name);  // val is runtime
    }
    total
}
```

#### `@field` intrinsic

`comptime_unroll for` over `@typeInfo` fields requires accessing struct fields by comptime-known name. The `@field` intrinsic provides this:

```gruel
@field(value, field_name)  // Access a field by comptime_str name
```

**Semantics:**
- `value` is a runtime value of struct type
- `field_name` is a `comptime_str` naming the field
- At compile time, `@field` resolves to a `FieldGet` instruction with the concrete field index
- Type-safe: the resolved field's type is used for type checking

This is equivalent to Zig's `@field`.

#### Implementation

`comptime_unroll for` is implemented as a desugaring pass in Sema, not as a new RIR instruction:

1. **Parser**: Parse `comptime_unroll for` as a new AST node `ComptimeUnrollFor { pattern, collection, body }`
2. **AstGen**: Lower to a new RIR instruction `ComptimeUnrollFor { binding, collection, body_start, body_len }`
3. **Sema**: When analyzing `ComptimeUnrollFor`:
   a. Evaluate the collection expression in comptime context → `ConstValue::Array(heap_idx)`
   b. Read the array from the comptime heap
   c. For each element, substitute the loop variable with the element's value and analyze the body
   d. Emit the analyzed body instructions for each iteration sequentially into the AIR

This means the loop is fully unrolled during sema — by the time the CFG is built, there is no loop, just N copies of the body with different constant substitutions.

#### Interaction with `@typeInfo` and `@field`

The canonical use case combines all three phases:

```gruel
fn serialize(comptime T: type, val: T) -> i32 {
    let info = @typeInfo(T);
    let mut hash: i32 = 0;
    comptime_unroll for field in info.fields {
        let field_val = @field(val, field.name);
        hash = hash * 31 + field_val;  // assuming i32 fields for simplicity
    }
    hash
}

struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let p = Point { x: 10, y: 20 };
    serialize(Point, p)
    // Unrolls to: hash = 0; hash = hash * 31 + p.x; hash = hash * 31 + p.y;
}
```

### New keywords and symbols

| Token | Type | Phase |
|-------|------|-------|
| `comptime_str` | Type keyword | 2 |
| `TypeKind` | Builtin enum | 3 |
| `comptime_unroll` | Keyword | 4 |
| `@compileError` | Intrinsic | 1 |
| `@compileLog` | Intrinsic | 1 |
| `@typeName` | TypeIntrinsic | 3 |
| `@typeInfo` | TypeIntrinsic | 3 |
| `@field` | Intrinsic | 4 |

## Implementation Phases

- [x] **Phase 1: `@compileError` and `@compileLog`** — Add `compileError` and `compileLog` to `KnownSymbols`. Handle `@compileError` in the comptime `Intrinsic` handler: evaluate the string literal argument and return `Err(CompileError::new(ErrorKind::ComptimeUserError(msg), span))`. Handle `@compileLog` by formatting all arguments and appending to a new `comptime_log_output: Vec<(String, Span)>` buffer on `Sema`; emit a warning after sema for each entry. Add `ErrorKind::ComptimeUserError(String)` to `gruel-error`. Add `format_const_value` helper method. Gate behind `comptime_meta` preview feature. Add spec rules for both intrinsics. Add spec tests.

- [ ] **Phase 2: `comptime_str` type** — Add `Type::ComptimeStr` tag to the type intern pool. Add `ConstValue::ComptimeStr(u32)` variant and `ComptimeHeapItem::String(String)` variant. Handle `StringConst` in `evaluate_comptime_inst` by promoting to `ComptimeStr`. Add `comptime_str` method dispatch in the `MethodCall` handler (len, is_empty, contains, starts_with, ends_with, eq, ne, lt, le, gt, ge). Add `==`, `!=`, `<`, `<=`, `>`, `>=` operator support for `ComptimeStr` pairs (lexicographic byte ordering for comparisons). Extend `@dbg`, `@compileError`, `@compileLog` to accept `comptime_str` values. Add legality check: reject `comptime_str` in runtime positions (same pattern as `type`). Gate behind `comptime_meta`. Add spec rules and tests.

- [ ] **Phase 3: `@typeInfo` and `@typeName`** — Add `TypeKind` builtin enum to `gruel-builtins` (variants: Struct, Enum, Int, Bool, Unit, Never, Array) and add it to `BUILTIN_ENUMS`. Add `@typeName` as a `TypeIntrinsic` handler in comptime evaluation: resolve type, format name, return `ComptimeStr`. Add `@typeInfo` as a `TypeIntrinsic` handler: resolve type, create anonymous struct types for the info structs (cache in `HashMap<Type, StructId>`), set `kind` field to the appropriate `TypeKind` variant, allocate instances on comptime heap with field values (using `ConstValue::ComptimeStr` for names, `ConstValue::Type` for type values, `ConstValue::Array` for field/variant lists), return `ConstValue::Struct`. Create `FieldInfo` and `VariantInfo` anonymous struct types. Handle struct, enum, and primitive type kinds. Gate behind `comptime_meta`. Add spec rules and tests.

- [ ] **Phase 4: `comptime_unroll for` and `@field`** — Add `comptime_unroll` keyword to the lexer. Add `ComptimeUnrollFor` AST node in parser (`comptime_unroll` followed by `for`, then the standard for-loop binding/iterable syntax). Lower to `ComptimeUnrollFor` RIR instruction in AstGen. In Sema, evaluate the collection expression in comptime context, then for each element: bind the loop variable as a comptime local and analyze the body, emitting the resulting AIR instructions. Support both comptime arrays and `@range` with comptime-known arguments as the iterable. Add `@field(value, field_name)` intrinsic: resolve `field_name` as a `comptime_str` to a concrete field index, emit a `FieldGet` instruction. Gate behind `comptime_meta`. Add spec rules and tests. Extend the differential fuzzer to generate `comptime_unroll for` programs.

## Consequences

### Positive

- **Full metaprogramming story**: Users can write generic serializers, validators, debug formatters, and type-safe builders using comptime
- **Better error messages**: Library authors can use `@compileError` to produce domain-specific compile errors instead of cryptic type mismatches
- **Debugging comptime**: `@compileLog` gives developers visibility into comptime execution without resorting to `@dbg` hacks
- **Incremental value**: Each phase is useful standalone — `@compileError` alone is worth the effort; `comptime_str` + `@typeInfo` unlocks reflection; `comptime_unroll for` enables code generation
- **Follows established patterns**: String-on-heap reuses the `ConstValue`/`ComptimeHeapItem` pattern; intrinsics reuse the `@dbg` handler pattern; anonymous structs reuse ADR-0025 Phase 4 infrastructure

### Negative

- **Interpreter complexity**: ~4 new intrinsic handlers, a new type (`comptime_str`), method dispatch for comptime strings, anonymous struct generation for `@typeInfo`, and loop unrolling logic for `comptime_unroll for`
- **Compile time impact**: `@typeInfo` constructs anonymous struct types on every invocation (mitigated by caching); `comptime_unroll for` duplicates body analysis N times
- **`comptime_str` is not `String`**: Two string types creates cognitive overhead. The distinction (`comptime_str` is compile-time only, `String` is runtime) is principled but users must learn it
- **New builtin enum**: `TypeKind` adds another compiler-injected enum (joining `Arch` and `Os`), but follows the established `BuiltinEnumDef` pattern
- **`@field` requires comptime name**: You cannot access a field by a runtime-computed name. This is by design (field access must be resolved at compile time for type safety) but may surprise users

### Neutral

- **No new runtime functions**: All changes are interpreter-only. The LLVM codegen is unaffected
- **New spec section**: `comptime_str`, `@typeInfo`, and `comptime_unroll for` extend spec section 4.14
- **Keyword budget**: One new keyword (`comptime_unroll`). The `comptime_` prefix signals this is a compile-time construct (consistent with `comptime_str`), and `unroll` communicates what it does — generating runtime code by unrolling over comptime data, which is distinct from a regular `for` loop running inside a `comptime` block
- **Comptime allocator deferred**: Explicitly not included; will be a separate ADR when needed

## Open Questions

1. **~~`comptime_str` equality semantics~~** *(resolved)*: `comptime_str` supports the full set of comparison operators (`==`, `!=`, `<`, `<=`, `>`, `>=`) with lexicographic byte ordering. This is trivial to implement (Rust `String`'s `Ord` impl) and avoids an artificial limitation that would need to be relaxed later.

2. **`@typeInfo` depth**: Should `@typeInfo` for a struct recursively include type info for its fields' types, or only include the `Type` value? Recursive expansion risks infinite loops for recursive types. Tentative answer: shallow — include `field_type: type` values; the user calls `@typeInfo` again on individual field types as needed.

3. **~~`comptime_unroll for` over ranges~~** *(resolved)*: `comptime_unroll for i in @range(N)` is the natural syntax, consistent with how regular for-loops already use `@range`. Both comptime arrays and `@range` with comptime-known arguments are supported as iterables.

4. **`@field` as lvalue**: Should `@field(val, name)` work on the left side of assignment (`@field(val, name) = 42`)? This would require `@field` to produce a place expression, not a value. Tentative answer: read-only initially; add lvalue support later if needed.

5. **`@compileLog` in production**: Should `@compileLog` be a hard error (not just warning) in release builds, or always a warning? Zig makes it a hard error. Tentative answer: warning only, to match Gruel's less-opinionated stance; users can add CI lints.

6. **`comptime_str` interning**: Should identical `comptime_str` values share the same heap slot (interning), or does each occurrence allocate independently? Interning saves memory but adds lookup overhead. Tentative answer: no interning initially; the comptime heap is short-lived (cleared per block) so the waste is bounded.

## Future Work

- **Comptime allocator**: Materializing comptime heap items (structs, arrays) into the compiled binary as static data, enabling comptime-computed lookup tables and configuration. Separate ADR.
- **`comptime_str` to `String` bridge**: A `to_static` method that materializes a `comptime_str` into a runtime `String` backed by `.rodata` (using the `cap = 0` convention from string literals). Requires the comptime allocator or a special codegen path.
- **`@embedFile`**: Reading files at compile time, returning `comptime_str`. Requires I/O policy decisions.
- **`@hasField` / `@hasMethod`**: Quick boolean intrinsics for checking type structure without full `@typeInfo`.
- **`comptime_unroll while`**: Compile-time unrolling of while loops with comptime conditions. Same unrolling machinery as `comptime_unroll for`.
- **Comptime arrays of `comptime_str`**: Currently the `split` method is deferred because it would return `[comptime_str; N]` where N is comptime-determined. This requires comptime array construction with `comptime_str` elements, which works naturally once both types are supported.

## References

- [ADR-0025: Compile-Time Execution](0025-comptime.md) — original comptime design
- [ADR-0033: LLVM Backend and Comptime Interpreter](0033-llvm-backend-and-comptime-interpreter.md) — interpreter implementation
- [ADR-0040: Comptime Interpreter Expansion](0040-comptime-expansion.md) — immediate predecessor; defined these as future work
- [Zig `@compileError`](https://ziglang.org/documentation/master/#compileError) — inspiration for Phase 1
- [Zig `@compileLog`](https://ziglang.org/documentation/master/#compileLog) — inspiration for Phase 1
- [Zig `@typeInfo`](https://ziglang.org/documentation/master/#typeInfo) — inspiration for Phase 3
- [Zig `inline for`](https://ziglang.org/documentation/master/#inline-for) — inspiration for Phase 4 (Gruel uses `comptime_unroll for` instead)
- [Zig `@field`](https://ziglang.org/documentation/master/#field) — inspiration for Phase 4

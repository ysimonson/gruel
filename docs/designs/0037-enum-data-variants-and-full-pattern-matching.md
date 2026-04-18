---
id: 0037
title: Enum Data Variants and Full Pattern Matching
status: proposal
tags: [types, syntax, pattern-matching, ownership]
feature-flag: enum_data_variants
created: 2026-04-18
accepted:
implemented:
spec-sections: ["4.7", "6.3"]
superseded-by:
---

# ADR-0037: Enum Data Variants and Full Pattern Matching

## Status

Proposal

## Summary

Extend Gruel enums to carry associated data per variant (algebraic data types), and extend pattern matching so match arm patterns can destructure and bind that data. This enables idiomatic sum types like `Option` and `Result` without generics, using concrete types initially.

## Context

### What Exists

ADR-0004 implemented C-style enums: discriminated unions with no per-variant data. Match expressions can branch on which variant is selected, but cannot extract any value from the variant:

```gruel
enum Color { Red, Green, Blue }
match c {
    Color::Red => 1,
    _ => 0,
}
```

ADR-0036 implemented struct let-destructuring, establishing the ownership framework for decomposing composite values into independently-owned fields.

### What's Missing

There is no way to associate data with a variant:

```gruel
// Not yet possible:
enum IntOption { Some(i32), None }
let x = IntOption::Some(42);
match x {
    IntOption::Some(v) => v,
    IntOption::None => 0,
}
```

This gap forces users to simulate sum types with structs + enums, sacrificing type safety and expressiveness. Sum types are fundamental to writing safe, expressive code without null pointers or error codes.

### Scope of This ADR

This ADR covers:
1. **Tuple-style data variants** — `Variant(T1, T2, ...)` in enum definitions
2. **Variant construction with data** — `Enum::Variant(expr1, expr2, ...)`
3. **Binding patterns in match arms** — `Enum::Variant(x, y) =>`
4. **Ownership of extracted data** — consistent with ADR-0036/ADR-0008

Out of scope (future ADRs):
- Generic enums (`Option<T>`, `Result<T, E>`)
- Struct-style variant data (`Variant { field: T }`)
- Nested patterns (`Some(Some(x))`)
- Or-patterns (`A | B =>`)
- Pattern matching in let bindings for enums

## Decision

### 1. Enum Variant Data Syntax

Variants may optionally carry a tuple of typed fields:

```gruel
enum IntOption {
    None,
    Some(i32),
}

enum Outcome {
    Ok(i32),
    Err(i32),
}

enum Tagged {
    Unit,
    One(i32),
    Two(i32, i32),
}
```

All existing C-style enums remain valid and unaffected.

#### Grammar Changes

```ebnf
enum_def       = "enum" IDENT "{" enum_variants "}" ;
enum_variants  = enum_variant { "," enum_variant } [ "," ] ;
enum_variant   = IDENT [ "(" type_list ")" ] ;    -- NEW: optional tuple data
type_list      = type { "," type } ;
```

#### Variant Construction

Data variants are constructed using a call-like syntax:

```gruel
let x = IntOption::Some(42);
let e = Outcome::Err(-1);
let t = Tagged::Two(10, 20);
```

Unit variants retain their existing syntax: `IntOption::None`, `Tagged::Unit`.

#### Updated Match Pattern Grammar

```ebnf
pattern        = "_" | INTEGER | BOOL | enum_variant_pattern | enum_data_pattern ;
enum_variant_pattern = IDENT "::" IDENT ;
enum_data_pattern    = IDENT "::" IDENT "(" binding_list ")" ;
binding_list   = binding { "," binding } ;
binding        = [ "mut" ] IDENT | "_" ;
```

Bindings in data patterns are simple identifiers or wildcards — no nested patterns in this ADR.

#### Match Arm Body Scoping

Each binding in a data pattern introduces a new local variable in the arm's body, with the type of the corresponding variant field:

```gruel
match x {
    IntOption::Some(v) => v + 1,   // v: i32
    IntOption::None => 0,
}

match t {
    Tagged::Two(a, b) => a + b,    // a: i32, b: i32
    Tagged::One(n) => n,
    Tagged::Unit => 0,
}
```

Wildcard `_` discards the field (immediately drops it if the type has a destructor):

```gruel
match x {
    IntOption::Some(_) => 1,
    IntOption::None => 0,
}
```

Mutability is per-binding:

```gruel
match x {
    IntOption::Some(mut v) => { v += 1; v },
    IntOption::None => 0,
}
```

#### Exhaustiveness

Data variants are treated the same as unit variants for exhaustiveness: an arm with `Enum::Variant(...)` (any binding pattern for each field) exhausts that variant. The binding contents do not affect exhaustiveness.

#### Ownership of Variant Data

When a data variant is matched and its fields are bound by name, those fields are **moved** out of the enum value. The enum value itself is consumed by the match expression (the scrutinee's slot is forgotten). This mirrors how struct destructuring works in let bindings (ADR-0036).

If a field is bound to `_`, it is immediately dropped (destructor runs if applicable).

**Copy types**: fields of copy types are copied out; the original enum value is still consumed at the match expression level (the scrutinee is used up).

**Non-copy types**: fields are moved out and become independent values owned by the arm body. If the arm body exits without consuming them, they are dropped at scope exit.

### 2. LLVM Memory Representation

#### Current Representation (C-style enums)

Currently, enums are represented as their discriminant integer (u8, u16, u32 as needed). No struct is created; the LLVM value is just `iN`.

#### New Representation (data variants)

For enums with at least one data variant, the LLVM type becomes a struct:

```
%EnumName = type { iD, [N x i8] }
```

Where:
- `iD` is the discriminant type (u8 for ≤256 variants, etc.)
- `N` is the size in bytes of the largest variant's payload, aligned to the largest field alignment

For unit-only enums (no data variants), the representation is unchanged (just `iD`).

**Rationale**: An opaque byte array with proper alignment is safe and avoids LLVM union complexity. Individual field reads/writes use `getelementptr` + `bitcast` to access the payload as the appropriate field type. This is the standard approach used by Rust's MIR-to-LLVM lowering.

#### Variant Construction

`Enum::Some(42)` lowers to:
1. `alloca %IntOption` to get a stack slot
2. Store discriminant `1` into field 0 (the `iD` field)
3. GEP into the payload byte array, bitcast to `i32*`, store `42`
4. Load the result as `%IntOption`

#### Match Dispatch

Match on data enums lowers to:
1. Extract the discriminant from field 0
2. Use LLVM `switch` on the discriminant value (as before)
3. In each arm's basic block, GEP into the payload to extract bound fields

#### Enums with No Data Variants

C-style enums retain their integer representation. No layout change. This ensures backward compatibility.

### 3. Type System Changes

`EnumDef` in `gruel-air/src/types.rs` is extended:

```rust
pub struct EnumDef {
    pub name: String,
    pub variants: Vec<EnumVariantDef>,  // was Vec<String>
    pub is_pub: bool,
    pub file_id: FileId,
}

pub struct EnumVariantDef {
    pub name: String,
    /// Field types for tuple-style data variants. Empty for unit variants.
    pub fields: Vec<Type>,
}

impl EnumDef {
    /// Whether any variant carries data.
    pub fn has_data_variants(&self) -> bool {
        self.variants.iter().any(|v| !v.fields.is_empty())
    }

    /// Whether the enum is a legacy C-style (all unit variants).
    pub fn is_unit_only(&self) -> bool {
        !self.has_data_variants()
    }
}
```

`AirPattern` is extended:

```rust
pub enum AirPattern {
    Wildcard,
    Int(i64),
    Bool(bool),
    EnumVariant { enum_id, variant_index },         // existing (unit variant)
    EnumDataVariant {                                // NEW
        enum_id: EnumId,
        variant_index: u32,
        bindings: Vec<Option<Symbol>>,  // None = wildcard
    },
}
```

`RirPattern` is extended similarly.

### 4. Interaction with Existing Features

**C-style enums**: fully backward compatible. Unit-only enums continue to be represented as integers.

**Match exhaustiveness**: unchanged logic — exhaustiveness is determined by variant coverage, not by data patterns.

**Struct methods on enums**: not affected; method dispatch continues to work.

**Copy vs non-copy**: data variant fields inherit their type's copy/move semantics. An enum with any non-copy field variant is itself non-copy.

**Destructors (ADR-0010)**: if an enum value with a data variant goes out of scope without being matched, its destructor must run. The destructor must check the discriminant and call the appropriate field destructors. This requires a new runtime pattern: a per-enum drop function that dispatches on discriminant.

## Implementation Phases

Epic: TBD

- [x] **Phase 1: Enum variant data declarations (parsing + type system)**
  - Parser: parse `Variant(Type, ...)` in enum definitions; add `fields: Vec<TypeExpr>` to AST enum variant
  - RIR: extend `InstData::EnumDecl` to store per-variant field types
  - AIR type system: replace `Vec<String>` with `Vec<EnumVariantDef>` in `EnumDef`; update all `EnumDef` construction sites
  - Sema gather pass: collect field types per variant; type-check them
  - Add `PreviewFeature::EnumDataVariants` in `gruel-error`
  - Gate the entire data-variant path behind this preview feature
  - No codegen yet — just ensure C-style enums still compile

- [x] **Phase 2: Variant construction with data**
  - Parser: parse `Enum::Variant(expr, ...)` as a variant construction call
  - RIR: `InstData::EnumVariantConstruct { type_name, variant, args: Vec<InstRef> }` (distinct from the existing unit `EnumVariant`)
  - AIR: new instruction `AirInstData::EnumCreate { enum_id, variant_index, fields: Vec<AirRef> }` (or extend existing)
  - LLVM codegen: change `gruel_type_to_llvm` for data enums to emit `{ iD, [N x i8] }`; implement construction via alloca + field stores
  - Unit-only enums retain integer representation
  - Add preview-gated spec tests: `Option::Some(42)` compiles but `exit_code` tests are not yet meaningful (no way to extract the value)

- [x] **Phase 3: Match patterns with binding**
  - Parser: parse `Enum::Variant(x, y)` and `Enum::Variant(mut z, _)` as patterns
  - RIR: `RirPattern::DataVariant { type_name, variant, bindings }` where each binding is `Option<(bool, Spur)>` (is_mut, name)
  - Sema: resolve pattern bindings; add bound variables to arm body scope; type-check each binding against the variant's field type
  - AIR: `AirPattern::EnumDataVariant { enum_id, variant_index, bindings: Vec<Option<Symbol>> }`
  - CFG: for each bound field, emit GEP into payload + field load into new StorageLive slot; forget the scrutinee slot (ownership transferred)
  - Wildcard fields: emit Drop if the field type has a destructor
  - Add `preview_should_pass = true` spec tests for basic binding

- [ ] **Phase 4: Drop dispatch for data enums**
  - Implement enum destructor dispatch: when a data enum value is dropped, the CFG must emit a match on the discriminant, then drop each live field of the matched variant
  - Update `gruel-cfg` drop logic: when dropping a value of an enum type that `has_data_variants()`, emit a `MatchDrop` sequence (or inline discriminant check + conditional field drops)
  - Add spec tests: non-copy data in enum variant is properly dropped at scope exit

- [ ] **Phase 5: LLVM layout correctness and ABI**
  - Verify that the `[N x i8]` payload is correctly sized and aligned for all field type combinations
  - Use LLVM `getelementptr` + appropriate pointer types for field access
  - Ensure that LLVM does not incorrectly alias the payload accesses (use `!noalias` if needed)
  - Add spec tests for multi-field variants, alignment-sensitive types

- [ ] **Phase 6: Spec, tests, stabilization**
  - Write spec paragraphs in `docs/spec/src/06-items/03-enums.md` for data variant declarations
  - Write spec paragraphs in `docs/spec/src/04-expressions/07-match-expressions.md` for binding patterns
  - Full test coverage: unit variants still work, data variants construct and match, ownership is correct, wildcards drop, mutability works
  - Run traceability check (`cargo run -p gruel-spec -- --traceability`)
  - When all tests pass and feature is stable: remove `preview` fields from spec tests, remove `require_preview()` call, remove `PreviewFeature::EnumDataVariants`

## Consequences

### Positive

- **Sum types**: users can define `Option`, `Result`, event enums, AST nodes — the cornerstone of safe, expressive programming
- **No null pointers**: `Option<T>` is the idiomatic alternative
- **Match exhaustiveness**: still enforced; compiler rejects non-exhaustive patterns
- **Backward compatible**: C-style enums are unchanged in representation and behavior
- **Foundation for generics**: once a type parameter system exists (separate ADR), making enums generic (`Option<T>`) becomes an extension of this work

### Negative

- **LLVM layout complexity**: the tagged union approach requires more codegen machinery than the current integer-only approach
- **Drop dispatch for enums**: adds a new drop pattern (conditional drop based on discriminant)
- **No nested patterns**: users must use intermediate `let` bindings for nested sum types until nested pattern matching is added

### Neutral

- **Non-generic only**: `Option` and `Result` must be defined with concrete types (`IntOption`, `StrResult`, etc.) until a generics ADR is implemented

## Resolved Questions

1. **Pattern arity mismatch**: `Variant(a, b)` used for a variant with 3 fields is an error. Should this be a parse-time or sema-time error? Sema-time is simpler (parser doesn't know field counts).

## Open Questions

1. **Enum drop dispatch strategy**: Should drop dispatch be emitted inline in the CFG (a `switch` on discriminant followed by per-variant field drops), or should a generated per-enum `__drop_EnumName` function be emitted and called? An inline approach is simpler initially; a function approach is more LLVM-IR-friendly (enables sharing and inlining decisions by LLVM). Start inline.

2. **Payload size calculation**: Should the payload size be computed at compile time in `gruel-air` (using type sizes), or delegated to the LLVM data layout? Computing it in `gruel-air` allows the type pool to know the enum's size, which may be needed for other purposes. Delegate to LLVM for now (LLVM knows target sizes).

## Future Work

- **Generic enums** (`Option<T>`, `Result<T, E>`) — requires a type parameter system (separate ADR)
- **Struct-style variant data** (`Variant { field: T }`) — straightforward extension once tuple-style is done
- **Nested patterns** (`Some(Some(x))`) — requires recursive pattern descent in sema
- **Or-patterns** (`A | B =>`) — independent extension to the pattern grammar
- **Pattern matching in let bindings** for enums (consistent with struct let-destructuring)
- **`if let`** syntax (`if let Some(x) = expr { ... }`) — syntactic sugar over `match`
- **Range patterns** for integers (`1..=5`)

## References

- [ADR-0004: Enum Types](0004-enum-types.md) — C-style enum foundation
- [ADR-0036: Struct Destructuring and Partial Move Ban](0036-destructuring-and-partial-move-ban.md) — Ownership model for destructuring
- [ADR-0008: Affine Types and MVS](0008-affine-types-mvs.md) — Ownership foundation
- [ADR-0010: Destructors](0010-destructors.md) — Drop infrastructure
- [ADR-0005: Preview Features](0005-preview-features.md) — Feature gating system
- [Rust Reference: Enum Types](https://doc.rust-lang.org/reference/items/enumerations.html)
- [Rust Reference: Patterns](https://doc.rust-lang.org/reference/patterns.html)
- [Austral Language: Sum Types](https://austral-lang.org/spec/spec.html#sum-types)

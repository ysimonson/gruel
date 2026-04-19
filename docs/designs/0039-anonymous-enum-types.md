---
id: 0039
title: Anonymous Enum Types
status: implemented
tags: [comptime, enums, generics]
feature-flag:
created: 2026-04-18
accepted:
implemented: 2026-04-19
spec-sections: ["4.14"]
superseded-by:
---

# ADR-0039: Anonymous Enum Types

## Status

Proposal

## Summary

Add anonymous enum type expressions (`enum { Variant1(T), Variant2, ... }`) analogous to existing anonymous struct type expressions. This enables comptime functions to construct enum types, unlocking generic sum types like `Option(T)`, `Result(T, E)`, and user-defined tagged unions.

## Context

Gruel already supports anonymous struct types as comptime type expressions (ADR-0025, ADR-0029), enabling generic product types:

```gruel
fn Pair(comptime T: type) -> type {
    struct { first: T, second: T }
}
```

However, there is no equivalent for sum types. Without anonymous enums, users cannot write generic `Option`, `Result`, or other tagged union types using comptime. Named enums exist (ADR-0037, ADR-0038) with full support for unit, tuple, and struct variants plus pattern matching, but they cannot be parameterized.

The struct and enum systems should be symmetric: if structs can be anonymous type expressions for comptime type construction, enums should too.

## Decision

### Syntax

Anonymous enum types use the same `enum { ... }` syntax as named enums, but appear as expressions (just like `struct { ... }` does for anonymous structs):

```gruel
fn Option(comptime T: type) -> type {
    enum {
        Some(T),
        None,
    }
}
```

All three variant forms are supported:

```gruel
fn Result(comptime T: type, comptime E: type) -> type {
    enum {
        Ok(T),           // tuple variant
        Err(E),          // tuple variant
    }
}

fn Shape() -> type {
    enum {
        Circle { radius: i32 },   // struct variant
        Rect { w: i32, h: i32 },  // struct variant
        Unit,                      // unit variant
    }
}
```

### EBNF Grammar

```ebnf
anon_enum_type = "enum" "{" variant { "," variant } [ "," ] "}" ;
variant = IDENT [ variant_fields ] ;
variant_fields = "(" type_list ")"                       (* tuple variant *)
               | "{" field_decl { "," field_decl } "}" ; (* struct variant *)
```

### Type Construction

Anonymous enum types are constructed via comptime functions that return `type`:

```gruel
fn Option(comptime T: type) -> type {
    enum {
        Some(T),
        None,
    }
}

fn main() -> i32 {
    let Opt = Option(i32);
    let x: Opt = Opt::Some(42);
    match x {
        Opt::Some(v) => v,
        Opt::None => 0,
    }
}
```

### Variant Construction

Anonymous enum variants are constructed with `Type::Variant(...)` syntax, just like named enums:

```gruel
let Opt = Option(i32);
let x = Opt::Some(42);       // tuple variant
let y = Opt::None;            // unit variant

let S = Shape();
let c = S::Circle { radius: 5 };  // struct variant
```

### Pattern Matching

Pattern matching works identically to named enums:

```gruel
let Opt = Option(i32);
let x: Opt = Opt::Some(42);
match x {
    Opt::Some(v) => v,
    Opt::None => 0,
}
```

### Structural Equality

Two anonymous enum types are structurally equal if and only if they have:

1. The same variant names in the same order
2. Each variant has the same form (unit, tuple, or struct)
3. Tuple variants have the same field types in the same order
4. Struct variants have the same field names and types in the same order
5. The same captured comptime values (same rule as anonymous structs)

```gruel
fn Opt1(comptime T: type) -> type { enum { Some(T), None } }
fn Opt2(comptime T: type) -> type { enum { Some(T), None } }

// Opt1(i32) and Opt2(i32) are the SAME type (structural equality)
// Opt1(i32) and Opt1(i64) are DIFFERENT types (different T)
```

### Methods

Anonymous enums support methods, following the same pattern as anonymous struct methods (ADR-0029):

```gruel
fn Option(comptime T: type) -> type {
    enum {
        Some(T),
        None,

        fn is_some(self) -> bool {
            match self {
                Self::Some(_) => true,
                Self::None => false,
            }
        }

        fn unwrap_or(self, default: T) -> T {
            match self {
                Self::Some(v) => v,
                Self::None => default,
            }
        }
    }
}
```

`Self` inside methods refers to the anonymous enum type, and can be used in patterns (`Self::Variant`), construction (`Self::Variant(value)`), and type annotations.

Method signatures (not bodies) participate in structural equality, same as anonymous structs.

### No Empty Enums

It is a compile-time error to define an anonymous enum with zero variants.

### Interaction with Existing Features

- **Drop**: Anonymous enums containing types with destructors dispatch drop correctly, using the same mechanism as named enums (discriminant-based dispatch).
- **Copy**: An anonymous enum is Copy if all variant field types are Copy, same as for anonymous structs.
- **Pattern exhaustiveness**: Match expressions on anonymous enums require exhaustive patterns, same as named enums.

## Implementation Phases

- [x] **Phase 1: Parser and RIR** — Add `TypeExpr::AnonymousEnum` to the parser AST. Add `InstData::AnonEnumType` to RIR. Implement parsing of `enum { Variant, Variant(T), Variant { field: T } }` as a type expression. Add RIR generation in `astgen.rs`.

- [x] **Phase 2: Sema (non-comptime path)** — Handle `AnonEnumType` in `analysis.rs` for direct anonymous enum type expressions (no comptime substitution). Implement `find_or_create_anon_enum()` in a new `anon_enums.rs` module with structural equality. Register anonymous enum in the type pool. Variant construction and pattern matching reuse existing named-enum code paths since the anonymous enum gets a real `EnumId`.

- [x] **Phase 3: Sema (comptime path)** — Handle `AnonEnumType` in `try_evaluate_const_with_subst()` with type/value substitution. Support comptime type parameters in variant fields. Register methods with `Self` resolution (similar to `register_anon_struct_methods_for_comptime_with_subst`).

- [x] **Phase 4: Spec and tests** — Add spec section 4.14 paragraphs for anonymous enum types. Add spec tests covering: basic construction, pattern matching, structural equality, methods, comptime parameterization, `Self` usage, error cases (empty enum, duplicate variants). Ensure traceability.

- [x] **Phase 5: Stabilization** — Remove preview gate. Remove `preview` field from spec tests. Update ADR status.

## Consequences

### Positive

- Enables generic sum types (`Option(T)`, `Result(T, E)`) via comptime — a critical building block for ergonomic Gruel programs.
- Symmetric with anonymous struct types — no conceptual gap where "structs can be generic but enums cannot."
- Reuses existing enum infrastructure (variant construction, pattern matching, drop dispatch, codegen) — anonymous enums become regular enums with generated names, just like anonymous structs.
- No new IR instruction kinds needed at the AIR/CFG/codegen level — `AnonEnumType` is purely a sema-level construct that produces a normal `EnumId`.

### Negative

- Adds complexity to the parser and sema for a second anonymous type form.
- Structural equality for enums is slightly more complex than for structs due to the three variant forms (unit, tuple, struct).
- Method resolution for `Self::Variant` inside anonymous enum methods requires special handling in sema.

## Resolved Questions

- Should anonymous enums support associated functions (non-`self` functions)? Anonymous structs do (spec rule 4.14:13). Yes for symmetry.

## Future Work

- **Trait-like dispatch**: Anonymous enum/struct methods are a stepping stone toward interface types.
- **Derive-like capabilities**: Auto-deriving `Debug`, `Eq`, etc. for anonymous types.

## References

- [ADR-0025: Compile-Time Execution (comptime)](0025-comptime.md)
- [ADR-0029: Anonymous Struct Methods](0029-anonymous-struct-methods.md)
- [ADR-0037: Enum Data Variants and Full Pattern Matching](0037-enum-data-variants-and-full-pattern-matching.md)
- [ADR-0038: Enum Struct Variants](0038-enum-struct-variants.md)
- Spec Section 4.14: Compile-Time Expressions

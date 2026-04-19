---
id: 0038
title: Enum Struct Variants (Named-Field Enum Variants)
status: proposal
tags: [types, syntax, pattern-matching, enums]
feature-flag: enum_struct_variants
created: 2026-04-18
accepted:
implemented:
spec-sections: ["6.3"]
superseded-by:
---

# ADR-0038: Enum Struct Variants (Named-Field Enum Variants)

## Status

Proposal

## Summary

Extend Gruel enums to support struct-like variants with named fields, in addition to the existing unit and tuple-style variants. This enables `Enum::Variant { field: value }` construction and `Enum::Variant { field }` pattern matching — the same syntax used for struct expressions, applied to enum variants.

## Context

### What Exists

ADR-0004 implemented C-style enums (unit variants only). ADR-0037 added tuple-style data variants:

```gruel
enum IntOption { Some(i32), None }

let x = IntOption::Some(42);
match x {
    IntOption::Some(v) => v,
    IntOption::None => 0,
}
```

Enum data variants have been stabilized as of ADR-0037 Phase 6.

### What's Missing

There is no way to define enum variants with named fields:

```gruel
// Not yet possible:
enum Shape {
    Circle { radius: i32 },
    Rectangle { width: i32, height: i32 },
    Point,
}

let s = Shape::Circle { radius: 5 };
match s {
    Shape::Circle { radius } => radius * radius,
    Shape::Rectangle { width, height } => width * height,
    Shape::Point => 0,
}
```

Named fields improve readability when variants carry multiple fields of the same type, where positional arguments are error-prone. Compare:

```gruel
// Tuple-style: which is width, which is height?
let r = Shape::Rectangle(10, 20);

// Struct-style: self-documenting
let r = Shape::Rectangle { width: 10, height: 20 };
```

### Scope of This ADR

This ADR covers:
1. **Struct-like variant definitions** — `Variant { field: Type, ... }` in enum declarations
2. **Struct-like variant construction** — `Enum::Variant { field: expr, ... }` expressions
3. **Struct-like variant pattern matching** — `Enum::Variant { field: binding, ... }` in match arms
4. **Field init shorthand** — `{ field }` means `{ field: field }` in both construction and patterns

Out of scope (future ADRs):
- Generic enum variants
- The `..` rest pattern for ignoring fields in match arms
- Nested patterns in bindings
- Update syntax (`Enum::Variant { field: new_val, ..existing }`)

## Decision

### 1. Three Variant Kinds

Each enum variant is exactly one of:

| Kind | Definition | Construction | Example |
|------|-----------|-------------|---------|
| Unit | `Variant` | `Enum::Variant` | `Color::Red` |
| Tuple | `Variant(T1, T2)` | `Enum::Variant(e1, e2)` | `Option::Some(42)` |
| Struct | `Variant { f: T }` | `Enum::Variant { f: e }` | `Shape::Circle { radius: 5 }` |

An enum can freely mix all three kinds:

```gruel
enum Message {
    Quit,                       // unit
    Echo(String),               // tuple
    Move { x: i32, y: i32 },   // struct
}
```

A variant with `( ... )` is always tuple-style. A variant with `{ ... }` is always struct-style. The two cannot be combined on a single variant.

### 2. Syntax

#### Definition Grammar

```ebnf
enum_variant   = IDENT [ variant_fields ] ;
variant_fields = "(" type_list ")"                    (* tuple variant *)
               | "{" struct_variant_fields "}" ;      (* struct variant *)
struct_variant_fields = struct_variant_field { "," struct_variant_field } [ "," ] ;
struct_variant_field  = IDENT ":" type ;
```

#### Construction Grammar

```ebnf
enum_struct_expr = IDENT "::" IDENT "{" [ field_inits ] "}" ;
field_inits      = field_init { "," field_init } [ "," ] ;
field_init       = IDENT ":" expression
                 | IDENT ;                            (* shorthand: name inferred from variable *)
```

Construction rules (same as struct expressions):
- All fields **MUST** be initialized — no partial initialization.
- Field initializers **MAY** be in any order.
- Field initializer expressions are evaluated left-to-right in source order.
- Field init shorthand: `{ x }` is equivalent to `{ x: x }`.

#### Pattern Grammar

```ebnf
enum_struct_pattern = IDENT "::" IDENT "{" [ field_patterns ] "}" ;
field_patterns      = field_pattern { "," field_pattern } [ "," ] ;
field_pattern       = IDENT ":" pattern_binding
                    | IDENT ;                          (* shorthand: bind to same name *)
pattern_binding     = "_" | [ "mut" ] IDENT ;
```

Pattern rules:
- All fields **MUST** be listed — no partial matching (no `..` in v1).
- Field patterns **MAY** be in any order.
- Field punning: `{ radius }` binds the `radius` field to variable `radius`.
- `{ radius: r }` binds the `radius` field to variable `r`.
- `{ radius: _ }` discards the `radius` field.
- `{ radius: mut r }` binds mutably.

### 3. Memory Layout

Struct variants use the **identical** memory layout as tuple variants — fields are stored sequentially in declaration order within the payload byte array. The name-to-index mapping is resolved entirely at compile time; there is no runtime difference between a struct variant and a tuple variant with the same field types in the same order.

This means struct variants inherit all layout properties from ADR-0037:
- `{ iD, [N x i8] }` tagged union representation
- Discriminant sizing (u8/u16/u32/u64)
- Packed payload with unaligned loads/stores
- Largest-variant payload sizing

### 4. Type System Changes

`EnumVariantDef` in `gruel-air/src/types.rs` is extended with optional field names:

```rust
pub struct EnumVariantDef {
    pub name: String,
    /// Field types. Empty for unit variants.
    pub fields: Vec<Type>,
    /// Field names for struct-like variants. Empty for unit and tuple variants.
    /// When non-empty, `field_names.len() == fields.len()`.
    pub field_names: Vec<String>,
}

impl EnumVariantDef {
    /// Whether this is a struct-like variant (has named fields).
    pub fn is_struct_variant(&self) -> bool {
        !self.field_names.is_empty()
    }

    /// Find a field by name (for struct variants). Returns the field index.
    pub fn find_field(&self, name: &str) -> Option<usize> {
        self.field_names.iter().position(|n| n == name)
    }
}
```

This is a minimal extension: tuple variants continue to have `field_names: vec![]`, and all existing code that only uses `fields` is unaffected.

### 5. Parser Changes

The parser's `IdentSuffix` enum gets a new variant for `::Variant { ... }`:

```rust
enum IdentSuffix {
    Call(Vec<CallArg>),
    StructLit(Vec<FieldInit>),
    Path(Ident),
    PathCall(Ident, Vec<CallArg>),
    PathStructLit(Ident, Vec<FieldInit>),  // NEW: ::Variant { field: expr }
    None,
}
```

There is no ambiguity because `::` clearly introduces a path, and the `{` after the variant name unambiguously starts a field list (as opposed to `(` for tuple construction or nothing for unit variants).

The AST `EnumVariant` node is extended to support named fields:

```rust
pub struct EnumVariant {
    pub name: Ident,
    pub kind: EnumVariantKind,
    pub span: Span,
}

pub enum EnumVariantKind {
    /// Unit variant: `Red`
    Unit,
    /// Tuple variant: `Some(i32)`
    Tuple(Vec<TypeExpr>),
    /// Struct variant: `Circle { radius: i32 }`
    Struct(Vec<EnumVariantField>),
}

pub struct EnumVariantField {
    pub name: Ident,
    pub ty: TypeExpr,
    pub span: Span,
}
```

A new `Pattern` variant is added for struct variant patterns:

```rust
pub enum Pattern {
    // ... existing variants ...
    /// Struct variant pattern (e.g., `Shape::Circle { radius }`)
    StructVariant {
        base: Option<Box<Expr>>,
        type_name: Ident,
        variant: Ident,
        fields: Vec<PatternFieldBinding>,
        span: Span,
    },
}

pub struct PatternFieldBinding {
    pub field_name: Ident,
    pub binding: PatternBinding,  // reuse existing type
}
```

### 6. Semantic Analysis

Construction of struct variants follows the same logic as struct expression analysis:

1. Look up the enum and variant by name
2. Verify the variant is a struct variant (has `field_names`)
3. Match each field initializer name to a declared field
4. Check for missing and duplicate fields
5. Type-check each field value against the declared type
6. Reorder fields to declaration order for the `EnumCreate` AIR instruction
7. Track source evaluation order (left-to-right)

Pattern matching follows similar logic:

1. Look up the enum and variant by name
2. Verify the variant is a struct variant
3. Match each field pattern name to a declared field
4. Check for missing and duplicate fields
5. Resolve field names to indices
6. Emit `EnumPayloadGet` with the resolved field indices

### 7. Codegen

No new codegen instructions are needed. Struct variants reuse:
- `EnumCreate` for construction (fields passed in declaration order)
- `EnumPayloadGet` for field extraction in patterns

The field name resolution happens entirely in sema; by the time we reach AIR/codegen, struct variants are identical to tuple variants.

### 8. Error Messages

New diagnostic cases:
- "unknown field `foo` in variant `Shape::Circle`"
- "missing field `radius` in variant `Shape::Circle`"
- "duplicate field `radius` in variant `Shape::Circle`"
- "variant `Shape::Circle` has named fields; use `Shape::Circle { ... }` instead of `Shape::Circle(...)`"
- "variant `Option::Some` has positional fields; use `Option::Some(...)` instead of `Option::Some { ... }`"

The last two catch attempts to use the wrong construction syntax for a variant's kind.

### 9. Interaction with Existing Features

**Tuple variants**: Unaffected. Tuple and struct variants are distinct at the definition site and cannot be mixed on the same variant.

**C-style/unit variants**: Unaffected.

**Drop dispatch (ADR-0037)**: Struct variants participate in drop dispatch identically to tuple variants — the discriminant check dispatches to per-variant drop code that drops each field.

**Ownership**: Same as tuple variants. When matched, fields are moved out. Wildcard `_` discards (drops). Copy types are copied.

**Exhaustiveness**: Struct variant patterns exhaust the variant regardless of field binding order. The field bindings don't affect exhaustiveness — only variant coverage matters.

## Implementation Phases

- [ ] **Phase 1: Struct variant declarations (parsing + type system)**
  - Extend AST `EnumVariant` with `EnumVariantKind` (unit/tuple/struct)
  - Parser: parse `Variant { field: Type, ... }` in enum definitions
  - RIR: extend enum variant encoding to store field names
  - AIR: add `field_names: Vec<String>` to `EnumVariantDef`
  - Sema gather: collect field names, check uniqueness, type-check field types
  - Add `PreviewFeature::EnumStructVariants` in `gruel-error`
  - Gate behind preview feature
  - Ensure existing enum tests still pass

- [ ] **Phase 2: Struct variant construction expressions**
  - Parser: add `PathStructLit(Ident, Vec<FieldInit>)` to `IdentSuffix`
  - AST: new `Expr` variant or extend existing for `Enum::Variant { ... }`
  - RIR: lower struct variant construction (reuse `EnumVariant` inst with field refs, or add a new instruction)
  - Sema: resolve field names to indices, type-check, reorder to declaration order
  - Support field init shorthand (`{ x }` means `{ x: x }`)
  - Support fields in any order
  - Error diagnostics for missing/unknown/duplicate fields
  - Error when using `( )` on a struct variant or `{ }` on a tuple variant
  - Codegen: no changes needed — `EnumCreate` already handles this

- [ ] **Phase 3: Struct variant pattern matching**
  - Parser: parse `Enum::Variant { field: binding }` patterns
  - AST: new `Pattern::StructVariant` variant
  - RIR: new `RirPattern::StructVariant` with named field bindings
  - AIR: extend `AirPattern` for struct variant patterns
  - Sema: resolve field names to indices, bind variables by name
  - Support field punning (`{ radius }` binds to variable `radius`)
  - All fields must be listed (no `..`)
  - Codegen: reuse `EnumPayloadGet` with resolved field indices

- [ ] **Phase 4: Spec, tests, and stabilization**
  - Update spec section 6.3 with struct variant rules
  - Update grammar appendix
  - Add comprehensive spec tests with traceability
  - Ensure `make test` passes with full traceability
  - Remove preview gate

## Consequences

### Positive

- Named fields make multi-field variants self-documenting and order-independent
- Consistent with struct expression syntax — users learn one pattern
- No new codegen complexity — struct variants are tuple variants with compile-time name resolution
- Minimal type system extension (just adding `field_names` to `EnumVariantDef`)

### Negative

- Three variant kinds increases the surface area of enum syntax
- More parser complexity (new `IdentSuffix` variant, new pattern kind)
- Error messages must distinguish tuple vs struct variants and guide users to the right syntax

## Open Questions

- Should field init shorthand be included from the start, or deferred? (Proposed: include from start for consistency with struct expressions.)
- Should we allow `..` in struct variant patterns to ignore remaining fields? (Proposed: not in v1, future ADR.)

## Future Work

- `..` rest pattern in struct variant patterns (ignore remaining fields)
- Nested patterns in field bindings (e.g., `Shape::Nested { inner: Shape::Circle { radius } }`)
- Update syntax for struct variants

## References

- [ADR-0004: Enum Types](0004-enum-types.md)
- [ADR-0037: Enum Data Variants and Full Pattern Matching](0037-enum-data-variants-and-full-pattern-matching.md)
- [ADR-0036: Destructuring and Partial Move Ban](0036-destructuring-and-partial-move-ban.md)

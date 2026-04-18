+++
title = "Enums"
weight = 3
template = "spec/page.html"
+++

# Enums

{{ rule(id="6.3:1", cat="normative") }}

An enum is defined using the `enum` keyword.

{{ rule(id="6.3:2", cat="normative") }}

```ebnf
enum_def = "enum" IDENT "{" [ enum_variants ] "}" ;
enum_variants = IDENT { "," IDENT } [ "," ] ;
```

## Enum Definition

{{ rule(id="6.3:3", cat="normative") }}

Variant names **MUST** be unique within an enum.

{{ rule(id="6.3:12", cat="normative") }}

An enum with zero variants is valid and represents an uninhabited type.
A zero-variant enum can never be constructed.

{{ rule(id="6.3:4", cat="normative") }}

Enum variants are referenced using path syntax: `EnumName::VariantName`.
An error is raised if the enum type does not exist.

{{ rule(id="6.3:5", cat="normative") }}

An error is raised if the variant does not exist within the enum.

{{ rule(id="6.3:6") }}

```gruel
enum Color {
    Red,
    Green,
    Blue,
}

fn main() -> i32 {
    let c = Color::Red;
    0
}
```

## Match on Enums

{{ rule(id="6.3:7", cat="normative") }}

Enum values can be matched using pattern matching in `match` expressions.
Each arm pattern uses the same path syntax as enum variant expressions.

{{ rule(id="6.3:8", cat="normative") }}

Match expressions on enums **MUST** be exhaustive: all variants **MUST** be covered,
either explicitly or via a wildcard pattern `_`.

{{ rule(id="6.3:9") }}

```gruel
enum Color { Red, Green, Blue }

fn main() -> i32 {
    let c = Color::Green;
    match c {
        Color::Red => 1,
        Color::Green => 2,
        Color::Blue => 3,
    }
}
```

## Enum Types

{{ rule(id="6.3:10", cat="normative") }}

Enums can be used as function parameter types, return types, and struct field types.

{{ rule(id="6.3:11") }}

```gruel
enum Color { Red, Green, Blue }

fn get_value(c: Color) -> i32 {
    match c {
        Color::Red => 1,
        Color::Green => 2,
        Color::Blue => 3,
    }
}
```

## Enum Data Variants

{{ rule(id="6.3:13", cat="normative") }}

Enum variants may optionally carry tuple-style associated data, declared as a
parenthesized list of types after the variant name. This feature requires the
`enum_data_variants` preview feature (`--preview enum_data_variants`).

```ebnf
enum_variant = IDENT [ "(" type_list ")" ] ;
type_list    = type { "," type } ;
```

Variants without a parenthesized type list are *unit variants* and carry no data.
Variants with a type list are *data variants* and carry one value per listed type.

{{ rule(id="6.3:14", cat="normative") }}

A data enum variant is constructed using the same path syntax as unit variants, but
followed by a parenthesized, comma-separated list of field value expressions:

```ebnf
enum_variant_expr = IDENT "::" IDENT "(" [ expr { "," expr } ] ")" ;
```

The number of arguments **MUST** equal the number of field types declared for that variant.
Each argument **MUST** match the corresponding declared field type.

{{ rule(id="6.3:15", cat="example") }}

```gruel
enum IntOption { Some(i32), None }

fn main() -> i32 {
    let x = IntOption::Some(42);
    0
}
```

## Data Variant Pattern Matching

{{ rule(id="6.3:16", cat="normative") }}

A data enum variant can be matched with a binding pattern in a `match` expression.
The binding pattern uses the same path syntax as the variant expression, followed
by a parenthesized list of bindings — one per field:

```ebnf
data_variant_pattern = IDENT "::" IDENT "(" [ binding { "," binding } ] ")" ;
binding = "_" | [ "mut" ] IDENT ;
```

Each named binding is introduced as a local variable in the match arm body,
with the type of the corresponding variant field.
Wildcard bindings (`_`) discard the field value.

{{ rule(id="6.3:17", cat="normative") }}

The number of bindings **MUST** equal the number of fields declared for that variant.

{{ rule(id="6.3:18", cat="example") }}

```gruel
enum IntOption { Some(i32), None }

fn get(opt: IntOption) -> i32 {
    match opt {
        IntOption::Some(v) => v,
        IntOption::None => 0,
    }
}
```

## Drop Dispatch for Data Enums

{{ rule(id="6.3:19", cat="normative") }}

When a data enum value goes out of scope without being matched, the compiler
emits a discriminant check and drops the fields of the active variant.
Only variants whose fields require drop are given a non-trivial drop body;
unit variants and variants with trivially-droppable fields are no-ops.

## Data Variant Layout

{{ rule(id="6.3:20", cat="normative") }}

A data enum is represented in memory as a tagged union: a discriminant integer
followed by a byte array large enough to hold the payload of the largest variant.
Fields within a variant are stored sequentially at consecutive byte offsets
(packed layout, no inter-field padding). Field accesses use unaligned loads and
stores, which are correct on all supported targets.

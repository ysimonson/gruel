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
enum_def       = "enum" IDENT "{" [ enum_variants ] "}" ;
enum_variants  = enum_variant { "," enum_variant } [ "," ] ;
enum_variant   = IDENT [ variant_fields ] ;
variant_fields = "(" type_list ")"
               | "{" struct_variant_fields "}" ;
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
parenthesized list of types after the variant name.

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

## Struct Variants

{{ rule(id="6.3:21", cat="normative") }}

Enum variants may carry struct-style associated data with named fields, declared
as a brace-enclosed list of `name: type` pairs after the variant name.

```ebnf
enum_variant        = IDENT [ variant_fields ] ;
variant_fields      = "(" type_list ")"                    (* tuple variant *)
                    | "{" struct_variant_fields "}" ;       (* struct variant *)
struct_variant_fields = struct_variant_field { "," struct_variant_field } [ "," ] ;
struct_variant_field  = IDENT ":" type ;
```

A variant with `( ... )` is a tuple variant. A variant with `{ ... }` is a
struct variant. A variant with neither is a unit variant. An enum may freely
mix all three kinds.

{{ rule(id="6.3:22", cat="legality-rule") }}

Field names within a struct variant **MUST** be unique. A compile-time error
is raised if a struct variant contains duplicate field names.

{{ rule(id="6.3:23", cat="normative") }}

Struct variants use the same memory layout as tuple variants — fields are stored
sequentially in declaration order. Name-to-index resolution happens entirely at
compile time; there is no runtime difference between a struct variant and a
tuple variant with the same field types in the same order.

{{ rule(id="6.3:24", cat="example") }}

```gruel
enum Shape {
    Circle { radius: i32 },
    Rectangle { width: i32, height: i32 },
    Point,
}
```

### Struct Variant Construction

{{ rule(id="6.3:25", cat="normative") }}

A struct variant is constructed using path syntax followed by a brace-enclosed
list of field initializers:

```ebnf
enum_struct_expr = IDENT "::" IDENT "{" [ field_inits ] "}" ;
field_inits      = field_init { "," field_init } [ "," ] ;
field_init       = IDENT ":" expression
                 | IDENT ;
```

All fields **MUST** be initialized — no partial initialization is allowed.
Field initializers **MAY** appear in any order. Field init shorthand `{ x }`
is equivalent to `{ x: x }`.

{{ rule(id="6.3:26", cat="legality-rule") }}

Using tuple-style construction `Enum::Variant(...)` on a struct variant, or
struct-style construction `Enum::Variant { ... }` on a tuple variant, is a
compile-time error.

{{ rule(id="6.3:27", cat="example") }}

```gruel
enum Shape {
    Circle { radius: i32 },
    Rectangle { width: i32, height: i32 },
    Point,
}

fn main() -> i32 {
    let radius = 5;
    let s = Shape::Circle { radius };
    0
}
```

### Struct Variant Pattern Matching

{{ rule(id="6.3:28", cat="normative") }}

A struct variant can be matched in a `match` expression using a brace-enclosed
list of field bindings:

```ebnf
enum_struct_pattern = IDENT "::" IDENT "{" [ field_patterns ] "}" ;
field_patterns      = field_pattern { "," field_pattern } [ "," ] ;
field_pattern       = IDENT ":" pattern_binding
                    | IDENT ;
pattern_binding     = "_" | [ "mut" ] IDENT ;
```

All fields **MUST** be listed — no partial matching is allowed.
Field patterns **MAY** appear in any order.
Field punning `{ radius }` binds the `radius` field to variable `radius`.
`{ radius: r }` binds the `radius` field to variable `r`.
`{ radius: _ }` discards the `radius` field.

{{ rule(id="6.3:29", cat="legality-rule") }}

Using struct-style pattern matching `Enum::Variant { ... }` on a tuple variant,
or tuple-style pattern matching `Enum::Variant(...)` on a struct variant, is a
compile-time error.

{{ rule(id="6.3:30", cat="example") }}

```gruel
enum Shape {
    Circle { radius: i32 },
    Rectangle { width: i32, height: i32 },
    Point,
}

fn area(s: Shape) -> i32 {
    match s {
        Shape::Circle { radius } => radius * radius,
        Shape::Rectangle { width, height } => width * height,
        Shape::Point => 0,
    }
}
```

## Inline Methods

{{ rule(id="6.3:31", cat="normative") }}

An enum **MAY** declare methods and associated functions inside its body, following the enum's variants.

{{ rule(id="6.3:32", cat="syntax") }}

```ebnf
enum_body     = [ enum_variants ] { method_def } ;
method_def    = [ directives ] "fn" IDENT "(" [ method_params ] ")" [ "->" type ] block ;
method_params = method_param { "," method_param } [ "," ] ;
method_param  = "self" | ( IDENT ":" type ) ;
```

{{ rule(id="6.3:33", cat="normative") }}

A method is a function in the enum body whose first parameter is `self`. An associated function is a function in the enum body with no `self` parameter.

{{ rule(id="6.3:34", cat="normative") }}

Methods are invoked with dot notation: `value.method(args)`. Associated functions are invoked with path notation: `EnumName::function(args)`. Method calls are desugared to ordinary function calls with the receiver as the first argument.

{{ rule(id="6.3:35", cat="legality-rule") }}

Method names within a single enum **MUST** be unique. Duplicate method names produce a compile-time error.

{{ rule(id="6.3:36", cat="example") }}

```gruel
enum Sign {
    Pos,
    Neg,
    Zero,

    fn from_i32(n: i32) -> Sign {
        if n > 0 {
            Sign::Pos
        } else if n < 0 {
            Sign::Neg
        } else {
            Sign::Zero
        }
    }

    fn to_i32(self) -> i32 {
        match self {
            Sign::Pos => 1,
            Sign::Neg => -1,
            Sign::Zero => 0,
        }
    }
}

fn main() -> i32 {
    let s = Sign::from_i32(-5);
    s.to_i32()
}
```
